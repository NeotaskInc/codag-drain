use crate::drain::Drain;
use crate::pipeline::ConcurrentDrain;

fn s(val: &str) -> String {
    val.to_string()
}

fn sv(vals: &[&str]) -> Vec<String> {
    vals.iter().map(|v| s(v)).collect()
}

#[test]
fn test_add_shorter_than_depth_message() {
    let mut model = Drain::new(4, 0.4, 100, None, vec![], s("<*>"), true);

    let (_, update) = model.add_log_message("hello");
    assert_eq!(update.as_str(), "cluster_created");

    let (_, update) = model.add_log_message("hello");
    assert_eq!(update.as_str(), "none");

    let (_, update) = model.add_log_message("otherword");
    assert_eq!(update.as_str(), "cluster_created");

    assert_eq!(model.cluster_count(), 2);
}

#[test]
fn test_add_log_message() {
    let mut model = Drain::default();
    let entries: Vec<&str> = vec![
        "",
        "            Dec 10 07:07:38 LabSZ sshd[24206]: input_userauth_request: invalid user test9 [preauth]",
        "            Dec 10 07:08:28 LabSZ sshd[24208]: input_userauth_request: invalid user webmaster [preauth]",
        "            Dec 10 09:12:32 LabSZ sshd[24490]: Failed password for invalid user ftpuser from 0.0.0.0 port 62891 ssh2",
        "            Dec 10 09:12:35 LabSZ sshd[24492]: Failed password for invalid user pi from 0.0.0.0 port 49289 ssh2",
        "            Dec 10 09:12:44 LabSZ sshd[24501]: Failed password for invalid user ftpuser from 0.0.0.0 port 60836 ssh2",
        "            Dec 10 07:28:03 LabSZ sshd[24245]: input_userauth_request: invalid user pgadmin [preauth]",
        "",
    ];
    let expected: Vec<&str> = vec![
        "",
        "            Dec 10 07:07:38 LabSZ sshd[24206]: input_userauth_request: invalid user test9 [preauth]",
        "            Dec 10 <*> LabSZ <*> input_userauth_request: invalid user <*> [preauth]",
        "            Dec 10 09:12:32 LabSZ sshd[24490]: Failed password for invalid user ftpuser from 0.0.0.0 port 62891 ssh2",
        "            Dec 10 <*> LabSZ <*> Failed password for invalid user <*> from 0.0.0.0 port <*> ssh2",
        "            Dec 10 <*> LabSZ <*> Failed password for invalid user <*> from 0.0.0.0 port <*> ssh2",
        "            Dec 10 <*> LabSZ <*> input_userauth_request: invalid user <*> [preauth]",
        "",
    ];
    let expected_trimmed: Vec<&str> = expected.iter().map(|s| s.trim()).collect();

    let mut actual: Vec<String> = Vec::new();
    for entry in &entries {
        let (cluster, _) = model.add_log_message(entry);
        actual.push(cluster.get_template());
    }

    assert_eq!(actual, expected_trimmed);
    assert_eq!(model.get_total_cluster_size(), 8);
}

#[test]
fn test_add_log_message_sim_75() {
    let mut model = Drain::new(4, 0.75, 100, None, vec![], s("<*>"), true);
    let entries: Vec<&str> = vec![
        "",
        "            Dec 10 07:07:38 LabSZ sshd[24206]: input_userauth_request: invalid user test9 [preauth]",
        "            Dec 10 07:08:28 LabSZ sshd[24208]: input_userauth_request: invalid user webmaster [preauth]",
        "            Dec 10 09:12:32 LabSZ sshd[24490]: Failed password for invalid user ftpuser from 0.0.0.0 port 62891 ssh2",
        "            Dec 10 09:12:35 LabSZ sshd[24492]: Failed password for invalid user pi from 0.0.0.0 port 49289 ssh2",
        "            Dec 10 09:12:44 LabSZ sshd[24501]: Failed password for invalid user ftpuser from 0.0.0.0 port 60836 ssh2",
        "            Dec 10 07:28:03 LabSZ sshd[24245]: input_userauth_request: invalid user pgadmin [preauth]",
        "",
    ];
    let expected: Vec<&str> = vec![
        "",
        "            Dec 10 07:07:38 LabSZ sshd[24206]: input_userauth_request: invalid user test9 [preauth]",
        "            Dec 10 07:08:28 LabSZ sshd[24208]: input_userauth_request: invalid user webmaster [preauth]",
        "            Dec 10 09:12:32 LabSZ sshd[24490]: Failed password for invalid user ftpuser from 0.0.0.0 port 62891 ssh2",
        "            Dec 10 <*> LabSZ <*> Failed password for invalid user <*> from 0.0.0.0 port <*> ssh2",
        "            Dec 10 <*> LabSZ <*> Failed password for invalid user <*> from 0.0.0.0 port <*> ssh2",
        "            Dec 10 07:28:03 LabSZ sshd[24245]: input_userauth_request: invalid user pgadmin [preauth]",
        "",
    ];
    let expected_trimmed: Vec<&str> = expected.iter().map(|s| s.trim()).collect();

    let mut actual: Vec<String> = Vec::new();
    for entry in &entries {
        let (cluster, _) = model.add_log_message(entry);
        actual.push(cluster.get_template());
    }

    assert_eq!(actual, expected_trimmed);
    assert_eq!(model.get_total_cluster_size(), 8);
}

#[test]
fn test_max_clusters() {
    let mut model = Drain::new(4, 0.4, 100, Some(1), vec![], s("<*>"), true);
    let entries = vec![
        "A format 1",
        "A format 2",
        "B format 1",
        "B format 2",
        "A format 3",
    ];
    let expected = vec![
        "A format 1",
        "A format <*>",
        "B format 1",
        "B format <*>",
        "A format 3",
    ];

    let mut actual: Vec<String> = Vec::new();
    for entry in &entries {
        let (cluster, _) = model.add_log_message(entry);
        actual.push(cluster.get_template());
    }

    assert_eq!(actual, expected);
    assert_eq!(model.get_total_cluster_size(), 1);
}

#[test]
fn test_max_clusters_lru_multiple_leaf_nodes() {
    let mut model = Drain::new(4, 0.4, 100, Some(2), vec![], s("*"), true);
    let entries = vec![
        "A A A", "A A B", "B A A", "B A B", "C A A", "C A B", "B A A", "A A A",
    ];
    let expected = vec![
        "A A A", "A A *", "B A A", "B A *", "C A A", "C A *", "B A *", "A A A",
    ];

    let mut actual: Vec<String> = Vec::new();
    for entry in &entries {
        let (cluster, _) = model.add_log_message(entry);
        actual.push(cluster.get_template());
    }

    assert_eq!(actual, expected);
    assert_eq!(model.get_total_cluster_size(), 4);
}

#[test]
fn test_max_clusters_lru_single_leaf_node() {
    let mut model = Drain::new(4, 0.4, 100, Some(2), vec![], s("*"), true);
    let entries = vec![
        "A A A", "A A B", "A B A", "A B B", "A C A", "A C B", "A B A", "A A A",
    ];
    let expected = vec![
        "A A A", "A A *", "A B A", "A B *", "A C A", "A C *", "A B *", "A A A",
    ];

    let mut actual: Vec<String> = Vec::new();
    for entry in &entries {
        let (cluster, _) = model.add_log_message(entry);
        actual.push(cluster.get_template());
    }

    assert_eq!(actual, expected);
}

#[test]
fn test_match_only() {
    let mut model = Drain::default();
    model.add_log_message("aa aa aa");
    model.add_log_message("aa aa bb");
    model.add_log_message("aa aa cc");
    model.add_log_message("xx yy zz");

    let c = model.match_default("aa aa tt");
    assert!(c.is_some());
    assert_eq!(c.unwrap().cluster_id, 1);

    let c = model.match_default("xx yy zz");
    assert!(c.is_some());
    assert_eq!(c.unwrap().cluster_id, 2);

    let c = model.match_default("xx yy rr");
    assert!(c.is_none());

    let c = model.match_default("nothing");
    assert!(c.is_none());
}

#[test]
fn test_create_template() {
    let model = Drain::new(4, 0.4, 100, None, vec![], s("*"), true);

    let seq1 = sv(&["aa", "bb", "dd"]);
    let seq2 = sv(&["aa", "bb", "cc"]);

    let template = model.create_template_pub(&seq1, &seq2);
    assert_eq!(template, sv(&["aa", "bb", "*"]));

    let template = model.create_template_pub(&seq1, &seq1);
    assert_eq!(template, seq1);
}

#[test]
#[should_panic]
fn test_create_template_unequal_lengths() {
    let model = Drain::new(4, 0.4, 100, None, vec![], s("*"), true);
    let seq1 = sv(&["aa", "bb", "dd"]);
    let seq3 = sv(&["aa"]);
    model.create_template_pub(&seq1, &seq3);
}

// ---------------------------------------------------------------------------
// Async / concurrent pipeline tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_concurrent_basic() {
    let drain = Drain::default();
    let cd = ConcurrentDrain::new(drain, 64);

    let entries: Vec<&str> = vec![
        "",
        "            Dec 10 07:07:38 LabSZ sshd[24206]: input_userauth_request: invalid user test9 [preauth]",
        "            Dec 10 07:08:28 LabSZ sshd[24208]: input_userauth_request: invalid user webmaster [preauth]",
        "            Dec 10 09:12:32 LabSZ sshd[24490]: Failed password for invalid user ftpuser from 0.0.0.0 port 62891 ssh2",
        "            Dec 10 09:12:35 LabSZ sshd[24492]: Failed password for invalid user pi from 0.0.0.0 port 49289 ssh2",
        "            Dec 10 09:12:44 LabSZ sshd[24501]: Failed password for invalid user ftpuser from 0.0.0.0 port 60836 ssh2",
        "            Dec 10 07:28:03 LabSZ sshd[24245]: input_userauth_request: invalid user pgadmin [preauth]",
        "",
    ];
    let expected: Vec<&str> = vec![
        "",
        "Dec 10 07:07:38 LabSZ sshd[24206]: input_userauth_request: invalid user test9 [preauth]",
        "Dec 10 <*> LabSZ <*> input_userauth_request: invalid user <*> [preauth]",
        "Dec 10 09:12:32 LabSZ sshd[24490]: Failed password for invalid user ftpuser from 0.0.0.0 port 62891 ssh2",
        "Dec 10 <*> LabSZ <*> Failed password for invalid user <*> from 0.0.0.0 port <*> ssh2",
        "Dec 10 <*> LabSZ <*> Failed password for invalid user <*> from 0.0.0.0 port <*> ssh2",
        "Dec 10 <*> LabSZ <*> input_userauth_request: invalid user <*> [preauth]",
        "",
    ];

    let mut actual: Vec<String> = Vec::new();
    for entry in &entries {
        let result = cd.add_log_message(entry).await;
        actual.push(result.template);
    }

    assert_eq!(actual, expected);

    cd.shutdown().await;
}

#[tokio::test]
async fn test_concurrent_ordering() {
    // Feed messages sequentially via await to preserve ordering and verify
    // deterministic output matches the synchronous Drain.
    let mut sync_drain = Drain::default();
    let drain = Drain::default();
    let cd = ConcurrentDrain::new(drain, 64);

    let entries = vec![
        "A format 1",
        "A format 2",
        "B format 1",
        "B format 2",
    ];

    for entry in &entries {
        let sync_result = sync_drain.add_log_message(entry);
        let async_result = cd.add_log_message(entry).await;

        assert_eq!(async_result.cluster_id, sync_result.0.cluster_id);
        assert_eq!(async_result.template, sync_result.0.get_template());
        assert_eq!(async_result.update_type, sync_result.1.as_str());
    }

    cd.shutdown().await;
}

#[tokio::test]
async fn test_concurrent_shutdown() {
    let drain = Drain::default();
    let cd = ConcurrentDrain::new(drain, 64);

    // Send a few messages, then shut down cleanly.
    cd.add_log_message("hello world").await;
    cd.add_log_message("hello world").await;

    // shutdown should complete without panic.
    cd.shutdown().await;
}
