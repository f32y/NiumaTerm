use std::hint::black_box;
use std::thread;
use std::time::{Duration, Instant};

use nmt_config::colors::Colors;

use crate::frame_source::TerminalFrameSource;
use crate::pane_model::FrameTheme;
use crate::session::{HostEvent, TerminalSessionConfig};

#[test]
#[ignore = "manual release-only surface frame profile"]
fn profile_surface_frame_pipeline() -> Result<(), &'static str> {
    if cfg!(debug_assertions) {
        return Err("run this profile with cargo test --release -p nmt_terminal_ui");
    }

    let theme = FrameTheme::default();
    let surface = TerminalFrameSource::new(
        TerminalSessionConfig {
            shell: Some("cmd.exe".into()),
            args: vec![
                "/D".into(),
                "/Q".into(),
                "/C".into(),
                "(for /L %i in (1,1,1000) do @echo %i terminal frame profile) & echo profile-ready"
                    .into(),
            ],
            cols: 80,
            rows: 24,
            ..TerminalSessionConfig::default()
        },
        1,
        None,
        Colors::default(),
    )
    .unwrap();

    let deadline = Instant::now() + Duration::from_secs(10);

    loop {
        if surface
            .session
            .poll_events()
            .iter()
            .any(|event| matches!(event, HostEvent::Exit))
        {
            break;
        }

        assert!(Instant::now() < deadline, "profile shell must finish");
        thread::sleep(Duration::from_millis(1));
    }

    let mut previous = surface.frame(None, &theme);
    assert!(
        previous
            .lines()
            .iter()
            .any(|line| line.text().contains("profile-ready"))
    );

    const WARMUP: usize = 256;
    const SAMPLES: usize = 10_000;
    let mut full = Duration::ZERO;
    let mut clean = Duration::ZERO;

    for sample in 0..WARMUP + SAMPLES {
        let start = Instant::now();
        let frame = black_box(surface.frame(None, &theme));
        let elapsed = start.elapsed();

        if sample >= WARMUP {
            full += elapsed;
        }
        assert_eq!(frame.lines().len(), 24);

        let start = Instant::now();
        let reused = black_box(surface.frame(Some(black_box(&previous)), &theme));
        let elapsed = start.elapsed();

        if sample >= WARMUP {
            clean += elapsed;
        }

        assert!(
            previous
                .lines()
                .iter()
                .zip(reused.lines())
                .all(|(old, new)| { old.cells().as_ptr() == new.cells().as_ptr() }),
            "a quiet session must retain every line allocation",
        );

        previous = reused;
    }

    eprintln!(
        "surface full={:?}/frame clean={:?}/frame (24/24 lines reused, no images)",
        full / SAMPLES as u32,
        clean / SAMPLES as u32,
    );
    Ok(())
}
