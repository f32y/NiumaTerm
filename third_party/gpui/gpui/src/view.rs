use crate::window::a11y::cache::{self, CachedA11y};
use crate::{
    AnyElement, AnyEntity, AnyWeakEntity, App, Bounds, ContentMask, Context, Element, ElementId,
    Entity, EntityId, GlobalElementId, InspectorElementId, IntoElement, LayoutId, PaintIndex,
    Pixels, PrepaintStateIndex, Render, RenderOnce, Style, StyleRefinement, TextStyle, WeakEntity,
};
use crate::{Empty, Window};
use crate::{FocusId, Point};
use anyhow::Result;
use collections::{FxHashSet, TypeIdHashSet};
use refineable::Refineable;
use std::mem;
use std::{any::TypeId, fmt, ops::Range};

/// A dynamically-typed view handle that can be downcast to a specific `Entity<V>`.
///
/// This is the type-erased counterpart to [`ViewElement`]: it holds an entity plus
/// a function pointer to its render, and is itself a [`View`], so embedding it as an
/// element goes through the same [`ViewElement`] machinery as any other view.
#[derive(Clone, Debug)]
pub struct AnyView {
    entity: AnyEntity,
    render: fn(&AnyView, &mut Window, &mut App) -> AnyElement,
}

impl<V: Render> From<Entity<V>> for AnyView {
    fn from(value: Entity<V>) -> Self {
        AnyView {
            entity: value.into_any(),
            render: any_view::render::<V>,
        }
    }
}

impl AnyView {
    /// Embed this view as a cached [`ViewElement`] laid out at `style`.
    ///
    /// The rendered subtree is recycled from the previous frame unless
    /// [Context::notify] was called on the backing entity or on a view inside
    /// it, or a global the subtree read was mutated, since it was rendered
    /// (or [Window::refresh] is called, which ignores caching).
    pub fn cached(self, style: StyleRefinement) -> ViewElement<AnyView> {
        ViewElement::new(self).cached(style)
    }

    /// Convert this to a weak handle.
    pub fn downgrade(&self) -> AnyWeakView {
        AnyWeakView {
            entity: self.entity.downgrade(),
            render: self.render,
        }
    }

    /// Convert this to a [Entity] of a specific type.
    /// If this handle does not contain a view of the specified type, returns itself in an `Err` variant.
    pub fn downcast<T: 'static>(self) -> Result<Entity<T>, Self> {
        match self.entity.downcast() {
            Ok(entity) => Ok(entity),
            Err(entity) => Err(Self {
                entity,
                render: self.render,
            }),
        }
    }

    /// Gets the [TypeId] of the underlying view.
    pub fn entity_type(&self) -> TypeId {
        self.entity.entity_type
    }

    /// The [`EntityId`] of this view.
    pub fn entity_id(&self) -> EntityId {
        self.entity.entity_id()
    }
}

impl PartialEq for AnyView {
    fn eq(&self, other: &Self) -> bool {
        self.entity == other.entity
    }
}

impl Eq for AnyView {}

/// `AnyView` is the type-erased [`View`]: its `render` is a function pointer rather
/// than a concrete type, but it participates in the reactive graph exactly like any
/// other view via [`ViewElement`].
impl View for AnyView {
    fn entity_id(&self) -> Option<EntityId> {
        Some(self.entity.entity_id())
    }

    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        (self.render)(&self, window, cx)
    }
}

impl<V: 'static + Render> IntoElement for Entity<V> {
    type Element = ViewElement<Entity<V>>;

    fn into_element(self) -> Self::Element {
        ViewElement::new(self)
    }
}

impl IntoElement for AnyView {
    type Element = ViewElement<AnyView>;

    fn into_element(self) -> Self::Element {
        ViewElement::new(self)
    }
}

/// A weak, dynamically-typed view handle.
pub struct AnyWeakView {
    entity: AnyWeakEntity,
    render: fn(&AnyView, &mut Window, &mut App) -> AnyElement,
}

impl AnyWeakView {
    /// Upgrade to a strong `AnyView` handle, if the view is still alive.
    pub fn upgrade(&self) -> Option<AnyView> {
        let entity = self.entity.upgrade()?;
        Some(AnyView {
            entity,
            render: self.render,
        })
    }
}

impl<V: 'static + Render> From<WeakEntity<V>> for AnyWeakView {
    fn from(view: WeakEntity<V>) -> Self {
        AnyWeakView {
            entity: view.into(),
            render: any_view::render::<V>,
        }
    }
}

impl PartialEq for AnyWeakView {
    fn eq(&self, other: &Self) -> bool {
        self.entity == other.entity
    }
}

impl std::fmt::Debug for AnyWeakView {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AnyWeakView")
            .field("entity_id", &self.entity.entity_id)
            .finish_non_exhaustive()
    }
}

mod any_view {
    use crate::{AnyElement, AnyView, App, IntoElement, Render, Window};

    pub(crate) fn render<V: 'static + Render>(
        view: &AnyView,
        window: &mut Window,
        cx: &mut App,
    ) -> AnyElement {
        let view = view.clone().downcast::<V>().unwrap();
        // Record the view's Render type name so the accessibility debug dump can
        // attribute nodes to the view that produced them.
        #[cfg(debug_assertions)]
        window
            .a11y
            .view_type_names
            .insert(view.entity_id(), std::any::type_name::<V>());
        view.update(cx, |view, cx| view.render(window, cx).into_any_element())
    }
}

/// A renderable that participates in GPUI's reactive graph — the unifying model
/// behind [`Render`] and [`RenderOnce`].
///
/// When `entity_id()` returns `Some`, that id becomes the view's identity: it gets
/// a unique element-id space (so internal `use_state` / `.id(..)` never collide
/// across siblings) and `cx.notify()` on that entity re-renders only this view's
/// subtree. `None` behaves like a stateless component.
///
/// You rarely implement `View` directly. `Entity<T: Render>` and any `T: RenderOnce`
/// get a blanket impl below; implement it by hand only when a component needs both
/// parent-supplied props *and* a backing entity for identity.
pub trait View: 'static + Sized {
    /// This view's identity, if it has one. A view typically holds the backing
    /// entity as a field and returns its [`EntityId`] here.
    ///
    /// The id becomes this view's [`ElementId`], so two views keyed on the same
    /// entity must not be rendered at the same position in the element tree
    /// (e.g. as siblings under the same parent): their internal element state
    /// (`use_state`, scroll offsets, etc.) would silently collide. Nesting is
    /// fine — the id is scoped by the parent path.
    fn entity_id(&self) -> Option<EntityId>;

    /// Render this view into an element tree, consuming `self`.
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement;
}

/// A stateless component (`RenderOnce`) is a `View` with no identity.
impl<T: RenderOnce> View for T {
    fn entity_id(&self) -> Option<EntityId> {
        None
    }

    #[inline]
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        RenderOnce::render(self, window, cx)
    }
}

/// An entity that renders itself (`Render`) is a `View` keyed on its own id.
impl<T: Render> View for Entity<T> {
    fn entity_id(&self) -> Option<EntityId> {
        Some(Entity::entity_id(self))
    }

    #[inline]
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        self.update(cx, |this, cx| {
            Render::render(this, window, cx).into_any_element()
        })
    }
}

impl<T: Render> Entity<T> {
    /// Cache a subtree clipped to its layout bounds. Fully visible subtrees can
    /// reuse drawing at a new position when their callbacks support offsets.
    /// Partly clipped content and unsupported handlers are rendered again.
    /// Custom elements must update retained geometry through
    /// [`Window::on_draw_with_offset`] and use
    /// [`Window::on_mouse_event_with_offset`] for handlers that capture bounds.
    /// Element event handlers must not capture absolute layout coordinates.
    #[track_caller]
    pub fn cached_clipped(self, style: StyleRefinement) -> ViewElement<Entity<T>> {
        let mut element = ViewElement::new(self).cached(style);
        element.clip = true;
        element
    }

    /// Embed this entity as a cached [`ViewElement`] laid out at `style`.
    ///
    /// The rendered subtree is reused until the entity or a view inside it is
    /// notified, a global it read is mutated, or the cached bounds / text
    /// style change. Caching requires a definite size:
    /// a cached view is laid out from `style` and is *not* measured from its
    /// contents. Use [`ViewElement::new`] (or `.child(entity)`) for the
    /// uncached case.
    #[track_caller]
    pub fn cached(self, style: StyleRefinement) -> ViewElement<Entity<T>> {
        ViewElement::new(self).cached(style)
    }
}

/// The element type for [`View`] implementations. Wraps a `View` and hooks it
/// into layout, prepaint, and paint. Constructed via [`ViewElement::new`].
#[doc(hidden)]
pub struct ViewElement<V: View> {
    view: Option<V>,
    entity_id: Option<EntityId>,
    cached_style: Option<StyleRefinement>,
    clip: bool,
    #[cfg(debug_assertions)]
    source: &'static core::panic::Location<'static>,
}

impl<V: View> ViewElement<V> {
    /// Wrap a [`View`] as an element.
    #[track_caller]
    pub fn new(view: V) -> Self {
        let entity_id = view.entity_id();
        ViewElement {
            entity_id,
            cached_style: None,
            clip: false,
            view: Some(view),
            #[cfg(debug_assertions)]
            source: core::panic::Location::caller(),
        }
    }

    /// Enable caching of this view's rendered subtree, laid out at `style`.
    /// The composer supplies the layout style because caching skips rendering
    /// the contents to measure them.
    ///
    /// Crate-private on purpose: caching is only sound for entity-backed views,
    /// where [`Context::notify`] is the contract that busts the cache. A stateless
    /// view has no such contract, so a frozen subtree could never be invalidated.
    /// Reach this through [`Entity::cached`] or [`AnyView::cached`], which are
    /// entity-backed by construction.
    pub(crate) fn cached(mut self, style: StyleRefinement) -> Self {
        self.cached_style = Some(style);
        self
    }
}

impl<V: View> IntoElement for ViewElement<V> {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

struct ViewElementState {
    prepaint_range: Range<PrepaintStateIndex>,
    paint_range: Range<PaintIndex>,
    cache_key: ViewElementCacheKey,
    accessed_entities: FxHashSet<EntityId>,
    accessed_globals: TypeIdHashSet,
    /// The global generation when the subtree finished rendering. Taken
    /// after the render so a global the subtree itself mutates while
    /// rendering does not bust its own cache on every frame.
    global_generation: u64,
    a11y: Option<CachedA11y>,
    paint_offset: Point<Pixels>,
}

struct ViewElementCacheKey {
    bounds: Bounds<Pixels>,
    content_mask: ContentMask<Pixels>,
    text_style: TextStyle,
    a11y_active: bool,
    focus: Option<FocusId>,
    clipped: bool,
    scale: f32,
}

impl ViewElementState {
    fn can_reuse(
        &self,
        next: &ViewElementCacheKey,
        entity_id: EntityId,
        window: &Window,
        cx: &App,
    ) -> bool {
        let previous = &self.cache_key;
        if previous.clipped != next.clipped
            || previous.scale != next.scale
            || previous.text_style != next.text_style
            || previous.a11y_active != next.a11y_active
            || (next.a11y_active && previous.focus != next.focus)
            || (next.a11y_active && window.cache_has_deferred_draws(&self.prepaint_range))
            || window.dirty_views.contains(&entity_id)
            || window.refreshing
            || cx.globals_changed_since(&self.accessed_globals, self.global_generation)
        {
            return false;
        }
        if previous.bounds == next.bounds && previous.content_mask == next.content_mask {
            return true;
        }

        // Glyph rasterization depends on the fractional device-pixel origin.
        // An integer translation preserves the selected atlas images.
        let offset = (next.bounds.origin - previous.bounds.origin).scale(next.scale);
        next.clipped
            && previous.bounds.size == next.bounds.size
            && previous.content_mask.bounds == previous.bounds
            && next.content_mask.bounds == next.bounds
            && offset.x.0.fract() == 0.
            && offset.y.0.fract() == 0.
            && self.a11y.as_ref().is_none_or(CachedA11y::can_translate)
            && window.can_translate_cache(&self.prepaint_range, &self.paint_range)
    }
}

impl<V: View> Element for ViewElement<V> {
    type RequestLayoutState = Option<AnyElement>;
    type PrepaintState = Option<AnyElement>;

    fn id(&self) -> Option<ElementId> {
        self.entity_id.map(ElementId::View)
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        #[cfg(debug_assertions)]
        return Some(self.source);

        #[cfg(not(debug_assertions))]
        return None;
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        if let Some(entity_id) = self.entity_id {
            // Stateful path: create a reactive boundary.
            window.with_rendered_view(entity_id, |window| {
                let caching_disabled = window.is_inspector_picking(cx);
                match self.cached_style.as_ref() {
                    Some(style) if !caching_disabled => {
                        let mut root_style = Style::default();
                        root_style.refine(style);
                        let layout_id = window.request_layout(root_style, None, cx);
                        (layout_id, None)
                    }
                    _ => {
                        let mut element = self
                            .view
                            .take()
                            .unwrap()
                            .render(window, cx)
                            .into_any_element();
                        let layout_id = element.request_layout(window, cx);
                        (layout_id, Some(element))
                    }
                }
            })
        } else {
            // Stateless path: isolate subtree via type name (no entity identity).
            window.with_id(
                ElementId::Name(std::any::type_name::<V>().into()),
                |window| {
                    let mut element = self
                        .view
                        .take()
                        .unwrap()
                        .render(window, cx)
                        .into_any_element();
                    let layout_id = element.request_layout(window, cx);
                    (layout_id, Some(element))
                },
            )
        }
    }

    fn prepaint(
        &mut self,
        global_id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        element: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<AnyElement> {
        if let Some(entity_id) = self.entity_id {
            // Stateful path.
            window.set_view_id(entity_id);
            window.with_rendered_view(entity_id, |window| {
                if let Some(mut element) = element.take() {
                    element.prepaint(window, cx);
                    return Some(element);
                }

                window.with_element_state::<ViewElementState, _>(
                    global_id.unwrap(),
                    |element_state, window| {
                        let mut content_mask = window.content_mask();
                        if self.clip {
                            content_mask.bounds = content_mask.bounds.intersect(&bounds);
                        }
                        let text_style = window.text_style();
                        let a11y_active = window.is_a11y_active();
                        let focus = window.focused(cx).map(|handle| handle.id);
                        let scale = window.scale_factor();
                        let cache_key = ViewElementCacheKey {
                            bounds,
                            content_mask,
                            text_style,
                            a11y_active,
                            focus,
                            clipped: self.clip,
                            scale,
                        };

                        if let Some(mut element_state) = element_state
                            && element_state.can_reuse(&cache_key, entity_id, window, cx)
                        {
                            let prepaint_start = window.prepaint_index();
                            let offset = bounds.origin - element_state.cache_key.bounds.origin;
                            window.reuse_prepaint_with_offset(
                                element_state.prepaint_range.clone(),
                                offset,
                                cx,
                            );
                            if let Some(a11y) = &mut element_state.a11y {
                                a11y.translate(offset, scale);
                                a11y.prepaint(&mut window.a11y);
                            }
                            element_state.paint_offset = offset;
                            element_state.cache_key.bounds = bounds;
                            element_state.cache_key.content_mask = content_mask;
                            cx.entities
                                .extend_accessed(&element_state.accessed_entities);
                            cx.extend_accessed_globals(&element_state.accessed_globals);
                            let prepaint_end = window.prepaint_index();
                            element_state.prepaint_range = prepaint_start..prepaint_end;

                            return (None, element_state);
                        }

                        let refreshing = mem::replace(&mut window.refreshing, true);
                        let prepaint_start = window.prepaint_index();
                        let a11y_start = cache::index(&window.a11y);
                        let ((mut element, accessed_entities), accessed_globals) = cx
                            .detect_accessed_globals(|cx| {
                                cx.detect_accessed_entities(|cx| {
                                    let mut element = self
                                        .view
                                        .take()
                                        .unwrap()
                                        .render(window, cx)
                                        .into_any_element();
                                    element.layout_as_root(bounds.size.into(), window, cx);
                                    window.with_content_mask(Some(content_mask), |window| {
                                        element.prepaint_at(bounds.origin, window, cx);
                                    });
                                    element
                                })
                            });
                        let global_generation = cx.global_generation();

                        let prepaint_end = window.prepaint_index();
                        window.refreshing = refreshing;

                        (
                            Some(element),
                            ViewElementState {
                                accessed_entities,
                                accessed_globals,
                                global_generation,
                                a11y: a11y_start.map(|start| cache::capture(&window.a11y, start)),
                                paint_offset: Point::default(),
                                prepaint_range: prepaint_start..prepaint_end,
                                paint_range: PaintIndex::default()..PaintIndex::default(),
                                cache_key,
                            },
                        )
                    },
                )
            })
        } else {
            // Stateless path: just prepaint the element.
            window.with_id(
                ElementId::Name(std::any::type_name::<V>().into()),
                |window| {
                    element.as_mut().unwrap().prepaint(window, cx);
                },
            );
            Some(element.take().unwrap())
        }
    }

    fn paint(
        &mut self,
        global_id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        element: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        if let Some(entity_id) = self.entity_id {
            // Stateful path.
            window.with_rendered_view(entity_id, |window| {
                let caching_disabled = window.is_inspector_picking(cx);
                if self.cached_style.is_some() && !caching_disabled {
                    window.with_element_state::<ViewElementState, _>(
                        global_id.unwrap(),
                        |element_state, window| {
                            let mut element_state = element_state.unwrap();

                            let paint_start = window.paint_index();

                            if let Some(element) = element {
                                let refreshing = mem::replace(&mut window.refreshing, true);
                                window.with_content_mask(
                                    Some(element_state.cache_key.content_mask),
                                    |window| {
                                        element.paint(window, cx);
                                    },
                                );
                                window.refreshing = refreshing;
                                if let Some(a11y) = &mut element_state.a11y {
                                    a11y.capture_actions(&window.a11y);
                                }
                            } else {
                                window.reuse_paint_with_offset(
                                    element_state.paint_range.clone(),
                                    element_state.paint_offset,
                                    cx,
                                );
                                if let Some(a11y) = &element_state.a11y {
                                    a11y.paint(&mut window.a11y);
                                }
                            }

                            let paint_end = window.paint_index();
                            element_state.paint_range = paint_start..paint_end;

                            ((), element_state)
                        },
                    )
                } else {
                    element.as_mut().unwrap().paint(window, cx);
                }
            });
        } else {
            // Stateless path: just paint the element.
            window.with_id(
                ElementId::Name(std::any::type_name::<V>().into()),
                |window| {
                    element.as_mut().unwrap().paint(window, cx);
                },
            );
        }
    }
}

/// A view that renders nothing
pub struct EmptyView;

impl Render for EmptyView {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        Empty
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        Context, Entity, Global, StyleRefinement, TestAppContext, Window, div, prelude::*, px, size,
    };
    use std::{cell::Cell, rc::Rc};

    struct Counter(usize);

    struct Label;

    impl Global for Label {}

    struct Unrelated;

    impl Global for Unrelated {}

    /// Reads a model and a global, and counts its own renders.
    struct CachedView {
        counter: Entity<Counter>,
        renders: Rc<Cell<usize>>,
    }

    impl Render for CachedView {
        fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            self.renders.set(self.renders.get() + 1);
            let _ = self.counter.read(cx).0;
            let _ = cx.global::<Label>();
            div().size_full()
        }
    }

    /// Reads the same model before the cached child renders, the way an
    /// ancestor that shows part of the same state would.
    struct RootView {
        counter: Entity<Counter>,
        child: Entity<CachedView>,
    }

    impl Render for RootView {
        fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let _ = self.counter.read(cx).0;
            div().size_full().child(
                self.child
                    .clone()
                    .cached(StyleRefinement::default().size_full()),
            )
        }
    }

    fn open(cx: &mut TestAppContext) -> (crate::WindowHandle<RootView>, Rc<Cell<usize>>) {
        cx.update(|cx| {
            cx.set_global(Label);
            cx.set_global(Unrelated);
        });
        let renders = Rc::new(Cell::new(0));
        let window = cx.open_window(size(px(400.), px(300.)), {
            let renders = renders.clone();
            move |_, cx| {
                let counter = cx.new(|_| Counter(0));
                let child = cx.new(|_| CachedView {
                    counter: counter.clone(),
                    renders,
                });
                RootView { counter, child }
            }
        });
        cx.run_until_parked();
        (window, renders)
    }

    #[gpui::test]
    fn test_cached_view_is_reused_when_only_its_parent_changes(cx: &mut TestAppContext) {
        let (window, renders) = open(cx);
        let before = renders.get();

        window.update(cx, |_, _, cx| cx.notify()).unwrap();
        cx.run_until_parked();

        assert_eq!(renders.get(), before);
    }

    #[gpui::test]
    fn test_cached_view_rerenders_only_for_globals_it_read(cx: &mut TestAppContext) {
        let (window, renders) = open(cx);
        let before = renders.get();

        window
            .update(cx, |_, _, cx| {
                cx.set_global(Unrelated);
                cx.notify();
            })
            .unwrap();
        cx.run_until_parked();
        assert_eq!(renders.get(), before);

        window
            .update(cx, |_, _, cx| {
                cx.set_global(Label);
                cx.notify();
            })
            .unwrap();
        cx.run_until_parked();
        assert_eq!(renders.get(), before + 1);
    }
}
