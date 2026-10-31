//! Explicit recorded-run integration diagnostic, not catalog-coupled regression coverage.
use super::*;

#[test]
#[ignore = "requires explicit preserved-run input; no provider calls"]
fn recorded_packet_cost_attribution() {
    let path = std::env::var("PLASM_PACKET_COST_INPUT").unwrap();
    let input: serde_json::Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    let catalogs = serde_json::from_value(input["catalogs"].clone()).unwrap();
    let deployments = serde_json::from_value(input["deployments"].clone()).unwrap();
    for (n, case) in input["cases"].as_array().unwrap().iter().enumerate() {
        let intent = serde_json::from_value(case["intent"].clone()).unwrap();
        let allowed = serde_json::from_value(case["allowed"].clone()).unwrap();
        let exposed: Vec<CapabilityRef> = serde_json::from_value(case["exposed"].clone()).unwrap();
        let visits = case["visits"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| {
                (
                    serde_json::from_value(v["reference"].clone()).unwrap(),
                    v["count"].as_u64().unwrap() as usize,
                )
            })
            .collect();
        let context = SearchContext {
            model: RELEVANCE_MODEL,
            intent: &intent,
            generation: case["generation"].as_str().unwrap(),
            catalogs: &catalogs,
            deployments: &deployments,
            allowed: &allowed,
            exposed: &exposed,
            visits: &visits,
        };
        let index = CatalogueSearch::new(&catalogs, &deployments, &allowed).unwrap();
        let roots = serde_json::from_value(case["roots"].clone()).unwrap();
        let plan = plan_recalled(&index, context, roots).unwrap();
        let actual = plan.issued.as_ref().unwrap().body().len();
        assert_eq!(
            actual,
            case["recorded_bytes"].as_u64().unwrap() as usize,
            "recorded byte parity case {n}"
        );
        for target in case["targets"].as_array().unwrap() {
            let target: CapabilityRef = serde_json::from_value(target.clone()).unwrap();
            let mut cache = BTreeMap::new();
            let solo = Bundle::from([(target.clone(), Vec::new())]);
            let solo_bytes = body_size(&solo, &index, context, &mut cache).unwrap();
            let mut selected = Bundle::new();
            let mut progression = Vec::new();
            for event in &plan.diagnostic {
                if event["kind"] == "initial_root" {
                    selected.insert(
                        serde_json::from_value(event["root"].clone()).unwrap(),
                        Vec::new(),
                    );
                } else {
                    for member in event["members"].as_array().unwrap() {
                        selected.insert(
                            serde_json::from_value(member["reference"].clone()).unwrap(),
                            serde_json::from_value(member["path"].clone()).unwrap(),
                        );
                    }
                }
                let mut with_target = selected.clone();
                with_target.insert(target.clone(), Vec::new());
                progression.push(serde_json::json!({"admission":event,
                    "with_target_bytes":body_size(&with_target,&index,context,&mut cache).unwrap()}));
            }
            println!(
                "PACKET_COST {}",
                serde_json::json!({"case":n,"target":target,
                "solo_bytes":solo_bytes,"recorded_bytes":actual,"progression":progression})
            );
        }
    }
}
