use std::fs;

use serde_json::Value;

use super::{DreamReplayArgs, run};

#[tokio::test]
async fn paired_replay_applies_one_merge_and_recovers_both_sources() {
    let out = tempfile::tempdir().unwrap();
    run(DreamReplayArgs {
        out: out.path().to_owned(),
    })
    .await
    .unwrap();
    let report: Value =
        serde_json::from_slice(&fs::read(out.path().join("receipt.json")).unwrap()).unwrap();

    assert_eq!(report["same_input"], true);
    assert_eq!(report["baseline_unchanged"], true);
    assert_eq!(report["plan_read_only"], true);
    assert_eq!(report["plan"]["merges"][0]["reason"], "planned_literal");
    assert_eq!(report["plan"]["clusters_merged"], 0);
    assert_eq!(report["apply"]["clusters_merged"], 1);
    assert_eq!(report["apply"]["inference_requests"], 0);
    let merge = &report["apply"]["merges"][0];
    assert_eq!(report["plan"]["merges"][0]["digest"], merge["digest"]);
    assert_eq!(report["restore"]["from"], merge["checkpoint"]);
    let sources = report["restore"]["sources"].as_array().unwrap();
    assert_eq!(sources.len(), 2);
    assert_eq!(sources[0]["author"], "ana-fixture");
    assert_eq!(sources[1]["author"], "bia-fixture");
    let tasks = report["tasks"].as_array().unwrap();
    assert_eq!(tasks.len(), 4);
    for (task, expected) in tasks.iter().zip([4, 2, 1, 1]) {
        for phase in ["baseline", "candidate", "restored"] {
            assert!(task[phase]["missing"].as_array().unwrap().is_empty());
            assert_eq!(task[phase]["evidence"].as_array().unwrap().len(), expected);
        }
        assert_eq!(task["baseline"]["context"], task["restored"]["context"]);
    }
    let combined = &tasks[0];
    assert_eq!(combined["baseline"]["context"]["pages"], 2);
    assert_eq!(combined["candidate"]["context"]["pages"], 1);
    assert_eq!(combined["baseline"]["context"]["utf8_bytes"], 2052);
    assert_eq!(combined["candidate"]["context"]["utf8_bytes"], 2054);
    assert_eq!(combined["candidate"]["context"]["estimated_tokens"], 498);
    assert_eq!(
        report["controls"]["lost_literal"].as_array().unwrap().len(),
        1
    );
    assert_eq!(
        report["controls"]["lost_source"].as_array().unwrap().len(),
        2
    );
    let markdown = fs::read_to_string(out.path().join("receipt.md")).unwrap();
    assert!(markdown.contains(merge["checkpoint"].as_str().unwrap()));
}
