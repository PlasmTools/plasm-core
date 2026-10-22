use super::*;
use futures_util::FutureExt;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

struct Outstanding(Arc<AtomicUsize>);
impl Drop for Outstanding {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

#[tokio::test]
async fn cancelling_a_route_drops_all_admitted_pages() {
    let (intent, retrieval) = many_cards(8);
    let outstanding = Arc::new(AtomicUsize::new(0));
    let admitted = AtomicUsize::new(0);
    let route = judge_candidates(RELEVANCE_MODEL, &intent, &retrieval, |_| {
        admitted.fetch_add(1, Ordering::SeqCst);
        outstanding.fetch_add(1, Ordering::SeqCst);
        let guard = Outstanding(outstanding.clone());
        async move {
            let _guard = guard;
            std::future::pending().await
        }
    });
    assert!(route.now_or_never().is_none());
    assert_eq!(admitted.load(Ordering::SeqCst), ROUTE_CONCURRENCY);
    assert_eq!(outstanding.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn invalid_page_aborts_without_a_partial_receipt_or_detached_work() {
    let (intent, retrieval) = many_cards(8);
    let outstanding = Arc::new(AtomicUsize::new(0));
    let mut admitted = 0;
    let result = judge_candidates(RELEVANCE_MODEL, &intent, &retrieval, |_| {
        admitted += 1;
        let reject = admitted == ROUTE_CONCURRENCY;
        outstanding.fetch_add(1, Ordering::SeqCst);
        let guard = Outstanding(outstanding.clone());
        async move {
            let _guard = guard;
            if reject {
                Ok("{}".to_owned())
            } else {
                std::future::pending().await
            }
        }
    })
    .await;
    assert!(matches!(result, Err(DiscoveryMatcherError::Response(_))));
    assert_eq!(admitted, ROUTE_CONCURRENCY);
    assert_eq!(outstanding.load(Ordering::SeqCst), 0);
}

#[test]
fn all_four_page_completion_permutations_preserve_mixed_choices() {
    let (intent, retrieval) = many_cards(4);
    let batches = issue_batches(RELEVANCE_MODEL, &intent, &retrieval).unwrap();
    let choices = [
        MatchChoice::Relevant,
        MatchChoice::Unrelated,
        MatchChoice::Uncertain,
        MatchChoice::Relevant,
    ];
    let decoded: Vec<_> = batches
        .iter()
        .zip(choices)
        .map(|(batch, choice)| decode_batch(batch, &response(batch, &[choice])).unwrap())
        .collect();
    let expected = finish_selection(decoded.concat(), &retrieval, &batches).unwrap();
    let mut permutations = 0;
    for a in 0..4 {
        for b in 0..4 {
            for c in 0..4 {
                for d in 0..4 {
                    let order = [a, b, c, d];
                    if order.into_iter().collect::<BTreeSet<_>>().len() != 4 {
                        continue;
                    }
                    let answers = order.into_iter().flat_map(|i| decoded[i].clone()).collect();
                    let issued = order
                        .into_iter()
                        .map(|i| batches[i].clone())
                        .collect::<Vec<_>>();
                    assert_eq!(
                        finish_selection(answers, &retrieval, &issued).unwrap(),
                        expected
                    );
                    permutations += 1;
                }
            }
        }
    }
    assert_eq!(permutations, 24);
}

pub(super) fn many_cards(count: usize) -> (IntentProvenance, RetrievalReceipt) {
    let (intent, mut retrieval) = fixture();
    let template = retrieval.candidates[0].clone();
    retrieval.candidates = (0..count)
        .map(|i| {
            let mut c = template.clone();
            c.id = format!("candidate{i}");
            c.reference.capability = format!("read{i}");
            // Each complete contract occupies its own page.
            let contract = "Complete abstract effect constraint. ";
            c.document.operation.contract =
                contract.repeat(TARGET_PAGE_BYTES / (2 * contract.len()) + 1);
            c
        })
        .collect();
    (intent, retrieval)
}

#[tokio::test]
async fn four_pages_progress_together_and_completion_order_does_not_change_receipt() {
    let (intent, retrieval) = many_cards(8);
    let batches = issue_batches(RELEVANCE_MODEL, &intent, &retrieval).unwrap();
    assert_eq!(batches.len(), 8);
    let barrier = Arc::new(tokio::sync::Barrier::new(ROUTE_CONCURRENCY));
    let active = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let result = judge_candidates(RELEVANCE_MODEL, &intent, &retrieval, |batch| {
        let barrier = barrier.clone();
        let active = active.clone();
        let peak = peak.clone();
        async move {
            let n = active.fetch_add(1, Ordering::SeqCst) + 1;
            peak.fetch_max(n, Ordering::SeqCst);
            barrier.wait().await;
            active.fetch_sub(1, Ordering::SeqCst);
            Ok(response(&batch, &[MatchChoice::Relevant]))
        }
    })
    .await
    .unwrap();
    assert_eq!(peak.load(Ordering::SeqCst), ROUTE_CONCURRENCY);
    let mut answers = Vec::new();
    for batch in batches.iter().rev() {
        answers.extend(decode_batch(batch, &response(batch, &[MatchChoice::Relevant])).unwrap());
    }
    assert_eq!(
        result,
        finish_selection(answers, &retrieval, &batches).unwrap()
    );
}

#[test]
fn shared_context_is_lossless_and_not_repeated_per_candidate() {
    let (_, mut retrieval) = many_cards(8);
    for candidate in &mut retrieval.candidates {
        candidate.document.operation.contract =
            "Affects the entire configured group; requires authorization.".into();
    }
    let turns = [
        "Inspect members",
        "Preserve unrelated members",
        "Revise: inspect only",
    ];
    let intent = IntentProvenance::from_turns(turns.map(str::to_owned)).unwrap();
    let batches = issue_batches(RELEVANCE_MODEL, &intent, &retrieval).unwrap();
    assert_eq!(batches.len(), 1);
    let content = question_content(&batches[0]);
    assert_eq!(content["context"]["intent_turns"], json!(turns));
    assert_eq!(content["context"]["rule"], RUBRIC);
    for question in content["questions"].as_object().unwrap().values() {
        assert!(question.get("criteria").is_none());
        assert!(question["instructions"].get("intent_turns").is_none());
        assert_eq!(
            question["instructions"]["capability"]["operation"]["contract"],
            "Affects the entire configured group; requires authorization."
        );
    }
    assert!(batches[0].body.len() <= TARGET_PAGE_BYTES);
    let repeated_bytes = serde_json::to_vec(&content["context"]).unwrap().len() * 7;
    assert!(repeated_bytes > 3000);
}
