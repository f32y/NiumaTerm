//! A blurred backdrop composed inside the window's own composition tree.
//!
//! DWM refuses to render a window-level system backdrop
//! (`DWMWA_SYSTEMBACKDROP_TYPE`) for a window that is not active, falling back to
//! a flat color instead. A popup that deliberately never takes activation — a
//! menu, whose owner has to keep its focused appearance — therefore cannot get its
//! material that way.
//!
//! Windows' own flyouts compose the material themselves: those windows are
//! `WS_EX_NOACTIVATE` and hold a host backdrop brush inside their composition
//! tree, which is not tied to activation. This does the same. The tree is
//!
//! ```text
//! DesktopWindowTarget
//! └── root (container)
//!     ├── backdrop (host backdrop brush)
//!     └── content  (the renderer's swap chain)
//! ```
//!
//! so anything the window paints with alpha composites onto the material rather
//! than replacing it, and the tint an acrylic recipe calls for is just a
//! translucent fill drawn by the window itself.

use std::cell::RefCell;
use std::mem::ManuallyDrop;

use anyhow::{Context as _, Result};
use windows::UI::Composition::Desktop::DesktopWindowTarget;
use windows::UI::Composition::{CompositionStretch, Compositor, ContainerVisual, SpriteVisual};
use windows::Win32::Foundation::{HWND, TRUE};
use windows::Win32::Graphics::Dwm::{DWMWA_USE_HOSTBACKDROPBRUSH, DwmSetWindowAttribute};
use windows::Win32::Graphics::Dxgi::IDXGISwapChain1;
use windows::Win32::System::WinRT::Composition::{ICompositorDesktopInterop, ICompositorInterop};
use windows::Win32::System::WinRT::{
    CreateDispatcherQueueController, DQTAT_COM_NONE, DQTYPE_THREAD_CURRENT, DispatcherQueueOptions,
};
use windows::core::Interface;
use windows_numerics::Vector2;

thread_local! {
    /// A `Compositor` needs a dispatcher queue on its thread, and one compositor
    /// serves every window on that thread, so both are created on first use.
    ///
    /// Never released: thread-local destructors run during thread teardown, by
    /// which point the apartment these objects belong to may already be gone, and
    /// releasing a composition object then faults inside the graphics stack. The
    /// process is exiting, so the memory is not worth the crash.
    static COMPOSITOR: RefCell<Option<ManuallyDrop<Compositor>>> = const { RefCell::new(None) };
}

fn compositor() -> Result<Compositor> {
    COMPOSITOR.with(|cell| {
        let mut cell = cell.borrow_mut();
        if let Some(compositor) = cell.as_ref() {
            return Ok((**compositor).clone());
        }

        let options = DispatcherQueueOptions {
            dwSize: std::mem::size_of::<DispatcherQueueOptions>() as u32,
            threadType: DQTYPE_THREAD_CURRENT,
            // The thread is already a single-threaded apartment, initialized by
            // the platform's `OleInitialize`. Asking the queue to initialize COM
            // as well leaves a second uninitialize pending against the same
            // apartment, which faults during process teardown.
            apartmentType: DQTAT_COM_NONE,
        };
        // The controller drives the queue from the thread's existing message
        // loop; leaking it is deliberate, since the queue has to outlive every
        // window drawn through this compositor.
        let controller = unsafe { CreateDispatcherQueueController(options) }
            .context("creating a dispatcher queue for the compositor")?;
        std::mem::forget(controller);

        let compositor = Compositor::new().context("creating the WinRT compositor")?;
        Ok((**cell.insert(ManuallyDrop::new(compositor))).clone())
    })
}

pub(crate) struct HostBackdropComposition {
    compositor: Compositor,
    /// Held because dropping the target tears the window's composition down.
    _target: DesktopWindowTarget,
    _root: ContainerVisual,
    content: SpriteVisual,
}

impl HostBackdropComposition {
    pub(crate) fn new(hwnd: HWND) -> Result<Self> {
        // A window that is not a UWP window produces an empty host backdrop
        // brush until it opts in, and the opt-in has to precede the brush.
        let enable = TRUE;
        unsafe {
            DwmSetWindowAttribute(
                hwnd,
                DWMWA_USE_HOSTBACKDROPBRUSH,
                (&raw const enable).cast(),
                std::mem::size_of_val(&enable) as u32,
            )
        }
        .context("opting the window into host backdrop brushes")?;

        let compositor = compositor()?;
        let interop: ICompositorDesktopInterop = compositor.cast()?;
        // Not a topmost target: the window's own z-order decides that, and
        // claiming it here would put the composition above unrelated windows.
        let target = unsafe { interop.CreateDesktopWindowTarget(hwnd, false) }
            .context("creating the window's composition target")?;

        let root = compositor.CreateContainerVisual()?;
        root.SetRelativeSizeAdjustment(Vector2 { X: 1.0, Y: 1.0 })?;

        let backdrop = compositor.CreateSpriteVisual()?;
        backdrop.SetBrush(&compositor.CreateHostBackdropBrush()?)?;
        backdrop.SetRelativeSizeAdjustment(Vector2 { X: 1.0, Y: 1.0 })?;

        let content = compositor.CreateSpriteVisual()?;
        content.SetRelativeSizeAdjustment(Vector2 { X: 1.0, Y: 1.0 })?;

        let children = root.Children()?;
        children.InsertAtTop(&backdrop)?;
        children.InsertAtTop(&content)?;
        target.SetRoot(&root)?;

        Ok(Self {
            compositor,
            _target: target,
            _root: root,
            content,
        })
    }

    pub(crate) fn set_swap_chain(&self, swap_chain: &IDXGISwapChain1) -> Result<()> {
        let interop: ICompositorInterop = self.compositor.cast()?;
        let surface = unsafe { interop.CreateCompositionSurfaceForSwapChain(swap_chain) }
            .context("wrapping the swap chain in a composition surface")?;

        let brush = self.compositor.CreateSurfaceBrushWithSurface(&surface)?;
        // The surface is always the window's size, so filling it keeps the
        // renderer's pixels one-to-one; the default uniform stretch would
        // letterbox any window whose aspect ratio differs from the surface.
        brush.SetStretch(CompositionStretch::Fill)?;
        self.content.SetBrush(&brush)?;

        Ok(())
    }
}
