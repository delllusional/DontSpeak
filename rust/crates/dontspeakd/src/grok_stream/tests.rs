//! grok_stream tests — tempdir only, no real `$HOME`.

use super::*;
use std::io::Write;

fn agent_line(session: &str, prompt: &str, text: &str) -> String {
    format!(
        r#"{{"timestamp":1,"method":"session/update","params":{{"sessionId":"{session}","update":{{"sessionUpdate":"agent_message_chunk","content":{{"type":"text","text":{text}}}}},"_meta":{{"promptId":"{prompt}"}}}}}}"#
        ,
        text = serde_json::to_string(text).unwrap()
    )
}

fn write_session_updates(paths: &Paths, session: &str, body: &str) -> PathBuf {
    let dir = ds_config::grok_session_dir(paths, "cwd-enc", session);
    std::fs::create_dir_all(&dir).unwrap();
    let path = ds_config::grok_updates_jsonl_path(&dir);
    std::fs::write(&path, body).unwrap();
    path
}

#[test]
fn registry_nudge_forget_and_park_snapshot() {
    let reg = SessionRegistry::new();
    assert_eq!(reg.len(), 0);
    reg.nudge("  ");
    assert_eq!(reg.len(), 0, "empty session ignored");
    reg.nudge("s1");
    reg.nudge("s2");
    assert!(reg.contains("s1"));
    assert_eq!(reg.len(), 2);
    let (snap, epoch) = reg.snapshot();
    assert_eq!(snap.len(), 2);
    assert!(epoch >= 2);
    reg.forget("s1");
    assert!(!reg.contains("s1"));
    assert!(reg.contains("s2"));
    reg.forget("missing");
    assert_eq!(reg.len(), 1);
}

#[test]
fn coalescer_flushes_on_newline_and_age() {
    let mut c = Coalescer::new();
    let t0 = Instant::now();
    assert!(c.on_delta("s", "k", "> half", t0).is_none());
    let (sess, batch) = c.on_delta("s", "k", " digest\n", t0).unwrap();
    assert_eq!(sess, "s");
    assert!(!batch.is_final);
    match &batch.payload {
        BatchPayload::Delta { text, .. } => assert_eq!(text, "> half digest\n"),
        other => panic!("expected delta, got {other:?}"),
    }
    // Age flush
    assert!(c.on_delta("s", "k2", "no-nl", t0).is_none());
    let aged = c.flush_aged(t0 + Duration::from_millis(200), Duration::from_millis(150));
    assert_eq!(aged.len(), 1);
    assert_eq!(aged[0].0, "s");
}

#[test]
fn integration_append_digest_chunks_batch_and_witness_silences_stop() {
    let dir = tempfile::tempdir().unwrap();
    let paths = Paths::rooted_at(dir.path());
    // Digests on by default.
    let cfg = VoiceConfig::default();
    assert!(cfg.grok_stream);
    assert!(cfg.narrates(NarrateKind::Digests));

    let session = "sess-int";
    // Pre-create empty updates so attach_at_eof starts at 0 of empty file.
    write_session_updates(&paths, session, "");

    let registry = SessionRegistry::new();
    registry.nudge(session);

    let mut attached = HashMap::new();
    let mut coalescer = Coalescer::new();
    let spoken = std::sync::Mutex::new(Vec::<String>::new());
    let mut speak = |sess: &str, u: &NarrationUtterance| {
        assert_eq!(sess, session);
        spoken.lock().unwrap().push(u.text.clone());
        Ok(())
    };
    let mic = || false;

    // Attach (EOF of empty file).
    poll_once_for_test(
        &paths,
        &registry,
        &mut attached,
        &mut coalescer,
        &cfg,
        &mic,
        &mut speak,
    );
    assert!(
        ds_narrate::witness_exists(&paths, session),
        "attach seeds witness"
    );
    assert!(spoken.lock().unwrap().is_empty());

    // Append digest chunks (partial then complete).
    let path = attached.get(session).unwrap().path.clone();
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap();
    writeln!(
        f,
        "{}",
        agent_line(session, "p1", "> Mid-turn digest line.\n\n")
    )
    .unwrap();
    // Extra non-agent line must be ignored.
    writeln!(
        f,
        r#"{{"method":"session/update","params":{{"update":{{"sessionUpdate":"agent_thought_chunk","content":{{"type":"text","text":"nope"}}}}}}}}"#
    )
    .unwrap();
    drop(f);

    poll_once_for_test(
        &paths,
        &registry,
        &mut attached,
        &mut coalescer,
        &cfg,
        &mic,
        &mut speak,
    );
    let spoken = spoken.into_inner().unwrap();
    assert!(
        spoken.iter().any(|s| s.contains("Mid-turn digest line")),
        "expected digest spoken, got {spoken:?}"
    );

    // Witness silences stop_utterances.
    assert!(ds_narrate::stop_utterances(
        Some("> Should not re-voice from Stop chat_history"),
        true,
        true,
        false,
        true
    )
    .is_empty());
}

#[test]
fn forget_drops_attachment_on_next_poll() {
    let dir = tempfile::tempdir().unwrap();
    let paths = Paths::rooted_at(dir.path());
    let cfg = VoiceConfig::default();
    let session = "sess-forget";
    write_session_updates(&paths, session, "");
    let registry = SessionRegistry::new();
    registry.nudge(session);
    let mut attached = HashMap::new();
    let mut coalescer = Coalescer::new();
    let mut speak = |_s: &str, _u: &NarrationUtterance| Ok(());
    let mic = || false;
    poll_once_for_test(
        &paths,
        &registry,
        &mut attached,
        &mut coalescer,
        &cfg,
        &mic,
        &mut speak,
    );
    assert_eq!(attached.len(), 1);
    registry.forget(session);
    poll_once_for_test(
        &paths,
        &registry,
        &mut attached,
        &mut coalescer,
        &cfg,
        &mic,
        &mut speak,
    );
    assert!(attached.is_empty());
}

#[test]
fn grok_stream_false_parks_and_clears_attachments() {
    let dir = tempfile::tempdir().unwrap();
    let paths = Paths::rooted_at(dir.path());
    let cfg = VoiceConfig {
        grok_stream: false,
        ..Default::default()
    };
    let session = "sess-park";
    write_session_updates(&paths, session, "");
    let registry = SessionRegistry::new();
    registry.nudge(session);
    let mut attached = HashMap::new();
    let mut coalescer = Coalescer::new();
    // Manually attach something so park must clear it.
    let path = ds_config::resolve_grok_updates_jsonl(&paths, session, None).unwrap();
    attached.insert(
        session.into(),
        Attached {
            tail: JsonlTail::attach_at_eof(path.clone()).unwrap(),
            path,
        },
    );
    let mut speak = |_s: &str, _u: &NarrationUtterance| Ok(());
    let mic = || false;
    poll_once_for_test(
        &paths,
        &registry,
        &mut attached,
        &mut coalescer,
        &cfg,
        &mic,
        &mut speak,
    );
    assert!(attached.is_empty(), "park clears attachments");
}
