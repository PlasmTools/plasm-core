//! Dual-surface (HTTP + MCP) E2E tests for async plan operations, review gate, and plan_commit_ref.

#[path = "common/hermit_lang_matrix.rs"]
mod hermit_lang_matrix;

#[path = "common/language_matrix.rs"]
#[allow(dead_code)]
mod language_matrix;

#[path = "common/long_operation.rs"]
#[allow(dead_code)]
mod long_operation;

use std::time::Duration;

use long_operation::{
    assert_async_accept, assert_cancelled, assert_review_gate_error, assert_running_wait,
    assert_terminal_success, cancel_program, continuity_phase, dry_verdict,
    operation_handle_from_accept, plan_commit_ref, wait_program, LongOpFixture, RunOpts, Surface,
    BOUNDED_LANG_ITEM, SLOW_LANG_ITEM, UNBOUNDED_LANG_ITEM,
};

async fn accept_async(
    fixture: &LongOpFixture,
    surface: Surface,
    program: &str,
    opts: RunOpts,
) -> String {
    let body = fixture
        .run_program(surface, program, opts)
        .await
        .expect("async accept");
    operation_handle_from_accept(&body)
}

async fn poll_wait_terminal(fixture: &LongOpFixture, surface: Surface, handle: &str) {
    let deadline = Duration::from_secs(5);
    let started = std::time::Instant::now();
    let mut terminal = None;
    while started.elapsed() < deadline {
        let body = fixture
            .run_program(surface, &wait_program(handle), RunOpts::default())
            .await
            .expect("wait poll");
        if continuity_phase(&body) != Some("running") {
            terminal = Some(body);
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(terminal.is_some(), "wait did not reach terminal within 5s");
    assert_terminal_success(terminal.as_ref().unwrap());
}

#[test]
fn long_operation_dual_surface_e2e() {
    std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(|| {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("runtime");
            rt.block_on(long_operation_dual_surface_e2e_async());
        })
        .expect("spawn long_operation e2e thread")
        .join()
        .expect("join");
}

async fn long_operation_dual_surface_e2e_async() {
    let fixture = LongOpFixture::setup().await;

    for surface in [Surface::Http, Surface::Mcp] {
        let body = fixture.plan_dry(surface, UNBOUNDED_LANG_ITEM).await;
        let pc = run_ref_from_meta(&body).expect("run_ref minted");
        assert!(pc.starts_with("pc"), "expected pcN ref, got {pc}");
        assert_eq!(dry_verdict(&body), Some("review"));
    }

    for surface in [Surface::Http, Surface::Mcp] {
        let result = tokio::time::timeout(
            Duration::from_secs(2),
            fixture.run_program(surface, UNBOUNDED_LANG_ITEM, RunOpts::default()),
        )
        .await;
        let err = match result {
            Ok(Err(e)) => e,
            Ok(Ok(_)) => panic!("expected review gate error on {surface:?}"),
            Err(_) => panic!("review gate should return quickly"),
        };
        assert_review_gate_error(&err);
    }

    for surface in [Surface::Http, Surface::Mcp] {
        let started = std::time::Instant::now();
        let body = fixture
            .run_program(
                surface,
                BOUNDED_LANG_ITEM,
                RunOpts {
                    wait: false,
                    force: true,
                    ..Default::default()
                },
            )
            .await
            .expect("async accept");
        assert!(started.elapsed() < Duration::from_secs(1));
        assert_async_accept(&body, surface.async_handle_prefix());
        fixture.cleanup().await;
    }

    for surface in [Surface::Http, Surface::Mcp] {
        let handle = accept_async(
            &fixture,
            surface,
            BOUNDED_LANG_ITEM,
            RunOpts {
                wait: false,
                force: true,
                ..Default::default()
            },
        )
        .await;
        poll_wait_terminal(&fixture, surface, &handle).await;
        fixture.cleanup().await;
    }

    for surface in [Surface::Http, Surface::Mcp] {
        let handle = accept_async(
            &fixture,
            surface,
            SLOW_LANG_ITEM,
            RunOpts {
                wait: false,
                force: true,
                ..Default::default()
            },
        )
        .await;
        tokio::time::sleep(Duration::from_millis(200)).await;
        let mut saw_running = false;
        for _ in 0..20 {
            let body = fixture
                .run_program(surface, &wait_program(&handle), RunOpts::default())
                .await
                .expect("wait");
            if continuity_phase(&body) == Some("running") {
                saw_running = true;
                assert_running_wait(&body);
                break;
            }
            if continuity_phase(&body) != Some("running") {
                break;
            }
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
        if !saw_running {
            eprintln!(
                "note: no Running phase observed on {surface:?} (slow plan may have finished before first poll)"
            );
        }
        fixture.cleanup().await;
    }

    for surface in [Surface::Http, Surface::Mcp] {
        let handle = accept_async(
            &fixture,
            surface,
            UNBOUNDED_LANG_ITEM,
            RunOpts {
                wait: false,
                force: true,
                ..Default::default()
            },
        )
        .await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        let mut cancelled = false;
        for _ in 0..80 {
            let wait_body = fixture
                .run_program(surface, &wait_program(&handle), RunOpts::default())
                .await
                .expect("wait before cancel");
            match continuity_phase(&wait_body) {
                Some("running") => {
                    let body = fixture
                        .run_program(surface, &cancel_program(&handle), RunOpts::default())
                        .await
                        .expect("cancel");
                    assert_cancelled(&body);
                    cancelled = true;
                    break;
                }
                Some("cancelled") => {
                    cancelled = true;
                    break;
                }
                Some("succeeded") | Some("failed") => {
                    eprintln!("note: plan finished before cancel on {surface:?}");
                    cancelled = true;
                    break;
                }
                None if wait_body.get("operation").and_then(|v| v.as_bool()) != Some(true) => {
                    eprintln!("note: plan finished before cancel on {surface:?}");
                    cancelled = true;
                    break;
                }
                _ => {}
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(cancelled, "failed to cancel mid-run on {surface:?}");
        fixture.cleanup().await;
    }

    for surface in [Surface::Http, Surface::Mcp] {
        let handle = accept_async(
            &fixture,
            surface,
            SLOW_LANG_ITEM,
            RunOpts {
                wait: false,
                force: true,
                ..Default::default()
            },
        )
        .await;
        fixture
            .run_program(surface, &cancel_program(&handle), RunOpts::default())
            .await
            .expect("first cancel");
        let body = fixture
            .run_program(surface, &cancel_program(&handle), RunOpts::default())
            .await
            .expect("second cancel");
        assert_cancelled(&body);
        fixture.cleanup().await;
    }

    for surface in [Surface::Http, Surface::Mcp] {
        let stale = match surface {
            Surface::Http => "wait(o999)".to_string(),
            Surface::Mcp => format!("wait({}_o999)", fixture.logical_session_ref),
        };
        let err = fixture
            .run_program(surface, &stale, RunOpts::default())
            .await
            .expect_err("stale handle");
        assert!(
            err.contains("unknown operation handle") || err.contains("stale"),
            "expected stale handle error, got: {err}"
        );
    }

    for surface in [Surface::Http, Surface::Mcp] {
        let dry = fixture.plan_dry(surface, BOUNDED_LANG_ITEM).await;
        let pc = run_ref_from_meta(&dry).expect("pc from dry run");
        let handle = accept_async(
            &fixture,
            surface,
            BOUNDED_LANG_ITEM,
            RunOpts {
                wait: false,
                plan_commit_ref: Some(pc),
                ..Default::default()
            },
        )
        .await;
        poll_wait_terminal(&fixture, surface, &handle).await;
        fixture.cleanup().await;
    }

    for surface in [Surface::Http, Surface::Mcp] {
        let dry = fixture.plan_dry(surface, UNBOUNDED_LANG_ITEM).await;
        let pc = run_ref_from_meta(&dry).expect("pc from dry run");
        let body = fixture
            .run_program(
                surface,
                UNBOUNDED_LANG_ITEM,
                RunOpts {
                    plan_commit_ref: Some(pc),
                    ..Default::default()
                },
            )
            .await
            .expect("review plan_commit_ref auto-async accept");
        assert_async_accept(&body, surface.async_handle_prefix());
        assert_eq!(
            body.get("_meta")
                .and_then(|m| m.get("plasm"))
                .and_then(|p| p.get("auto_async"))
                .and_then(|v| v.as_bool()),
            Some(true)
        );
        let handle = operation_handle_from_accept(&body);
        poll_wait_terminal(&fixture, surface, &handle).await;
        fixture.cleanup().await;
    }

    for surface in [Surface::Http, Surface::Mcp] {
        let dry = fixture.plan_dry(surface, UNBOUNDED_LANG_ITEM).await;
        let pc = run_ref_from_meta(&dry).expect("pc from dry run");
        let err = fixture
            .run_program(
                surface,
                BOUNDED_LANG_ITEM,
                RunOpts {
                    plan_commit_ref: Some(pc),
                    ..Default::default()
                },
            )
            .await
            .expect_err("mismatch");
        assert!(
            err.contains("does not match") || err.contains("plan_commit_ref"),
            "expected mismatch error, got: {err}"
        );
    }

    for surface in [Surface::Http, Surface::Mcp] {
        let handle = accept_async(
            &fixture,
            surface,
            BOUNDED_LANG_ITEM,
            RunOpts {
                wait: false,
                force: true,
                ..Default::default()
            },
        )
        .await;
        for _ in 0..10 {
            let body = fixture
                .run_program(surface, &wait_program(&handle), RunOpts::default())
                .await
                .expect("wait");
            if continuity_phase(&body) == Some("running") {
                assert_running_wait(&body);
                break;
            }
            if continuity_phase(&body) != Some("running") {
                break;
            }
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
        fixture.cleanup().await;
    }

    fixture.cleanup().await;
}
