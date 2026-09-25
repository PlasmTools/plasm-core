//! Keep this in its own test process: another registered subscriber masks the
//! single-dispatcher callsite-registration bug this test exercises.
#![cfg(feature = "testing")]

use plasm_otel::span_capture::{find_span, is_child_of, with_captured_spans};

fn shared_request(request: i64) {
    let parent = tracing::debug_span!("capture.operation", request);
    let _entered = parent.enter();
    tracing::debug_span!("capture.http").in_scope(|| {});
}

#[test]
fn untraced_thread_cannot_disable_capture_callsites() {
    let ((), spans) = with_captured_spans(|| {
        // Force the first use of both shared callsites onto an untraced thread
        // while the capturing thread's subscriber is alive.
        std::thread::spawn(|| shared_request(-1)).join().unwrap();
        shared_request(0);
    });
    let parent = find_span(&spans, "capture.operation").expect("captured parent");
    let child = find_span(&spans, "capture.http").expect("captured HTTP child");
    assert!(is_child_of(child, parent));
    assert_eq!(
        spans.len(),
        2,
        "untraced request must not enter the capture"
    );

    // Captures must also remain isolated while other captures are installed and
    // dropped, and while untraced requests continue using the same callsites.
    let start = std::sync::Barrier::new(5);
    std::thread::scope(|scope| {
        let start = &start;
        let untraced = scope.spawn(move || {
            start.wait();
            for _ in 0..256 {
                shared_request(-1);
            }
        });
        let workers: Vec<_> = (1..=4)
            .map(|worker| {
                scope.spawn(move || {
                    start.wait();
                    for iteration in 0..32 {
                        let request = worker * 32 + iteration;
                        let ((), spans) = with_captured_spans(|| shared_request(request));
                        let parent = find_span(&spans, "capture.operation").expect("parent");
                        let child = find_span(&spans, "capture.http").expect("child");
                        assert!(is_child_of(child, parent));
                        assert_eq!(spans.len(), 2);
                        assert!(parent.attributes.iter().any(|attribute| {
                            attribute.key.as_str() == "request"
                                && attribute.value == opentelemetry::Value::I64(request)
                        }));
                    }
                })
            })
            .collect();
        untraced.join().unwrap();
        for worker in workers {
            worker.join().unwrap();
        }
    });
}
