use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// Production `main` → `ds_log::init()`. `DONTSPEAK_LOG_FILE` redirects the unified log into
/// a tempdir (HOME/LOCALAPPDATA overrides don't work cross-platform — Windows known-folder
/// APIs ignore child env for LOCALAPPDATA). See issues #26 and #187.
#[test]
fn sinkless_helper_rejects_tts_load_and_speak() {
    let home = tempfile::tempdir().expect("tempdir");
    let log_path = home.path().join("dontspeak.log");

    let mut child = Command::new(env!("CARGO_BIN_EXE_ds-helper"))
        .arg("--serve")
        .env("DONTSPEAK_LOG_FILE", &log_path)
        .env_remove("DONTSPEAK_TTS_PRELOAD")
        .env_remove("DONTSPEAK_STT_PRELOAD")
        .env_remove("DONTSPEAK_FULL_DUPLEX")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn sink-less helper");
    let stdout = child.stdout.take().expect("piped helper stdout");
    let (tx, rx) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    let recv = || {
        rx.recv_timeout(Duration::from_secs(10))
            .expect("helper protocol reply timed out")
            .expect("read helper protocol reply")
    };

    let ready = recv();
    let mut stdin = child.stdin.take().expect("piped helper stdin");
    writeln!(stdin, r#"{{"op":"load","engine":"tts"}}"#).unwrap();
    let load = recv();
    writeln!(
        stdin,
        r#"{{"op":"speak","voice":"af_sarah","language":"en","rate":1.0,"text":"hello"}}"#
    )
    .unwrap();
    let speak = recv();

    // Unload always logs `unloaded tts, freed=…` (even when load failed). Keep stdin open
    // until the log line appears — stdin EOF clears pending unload before the job runs.
    writeln!(stdin, r#"{{"op":"unload","engine":"tts"}}"#).unwrap();
    let _ = stdin.flush();
    let start = Instant::now();
    loop {
        if log_path.is_file() {
            let log = std::fs::read_to_string(&log_path).unwrap_or_default();
            if log.contains("unloaded tts") {
                break;
            }
        }
        if start.elapsed() > Duration::from_secs(10) {
            let log = std::fs::read_to_string(&log_path).unwrap_or_default();
            panic!(
                "helper did not write redirected unload log within 10s; log={log:?} path={}",
                log_path.display()
            );
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    drop(stdin);
    let status = child.wait().expect("wait for helper EOF exit");
    reader.join().expect("join helper stdout reader");

    assert_eq!(ready, ds_helper_proto::READY);
    assert_eq!(
        load,
        "TTSLOADERR helper started without TTS output; restart required"
    );
    assert_eq!(
        speak,
        "ERR helper started without TTS output; restart required"
    );
    assert!(status.success(), "helper EOF exit failed: {status}");
}
