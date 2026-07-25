//! Session-scoped lifecycle for external dictation presenters.

fn parse_value(value: Option<&String>, option: &str) -> Result<String, String> {
    let value = value
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("{option} requires a non-empty value"))?;
    if value.len() > 120
        || !value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
    {
        return Err(format!(
            "{option} accepts letters, digits, '-', '_' and '.' only"
        ));
    }
    Ok(value.to_string())
}

fn option(args: &[String], option: &str) -> Result<String, String> {
    let mut values = args
        .windows(2)
        .filter(|pair| pair[0] == option)
        .map(|pair| &pair[1]);
    let value = parse_value(values.next(), option)?;
    if values.next().is_some() {
        return Err(format!("{option} must not be repeated"));
    }
    Ok(value)
}

fn ttl(args: &[String]) -> Result<u64, String> {
    let ttl = option(args, "--ttl-ms")?
        .parse()
        .map_err(|_| "--ttl-ms requires a positive integer".to_string())?;
    if !(500..=60_000).contains(&ttl) {
        return Err("--ttl-ms must be between 500 and 60000".into());
    }
    Ok(ttl)
}

fn validate_options(args: &[String], allowed: &[&str]) -> Result<(), String> {
    if !args.len().is_multiple_of(2) {
        return Err("presenter options require a value".into());
    }
    for pair in args.chunks_exact(2) {
        let option = pair[0].as_str();
        if !allowed.contains(&option) {
            return Err(format!("unknown presenter option {option:?}"));
        }
    }
    Ok(())
}

fn response(request: &ds_ipc::Request) -> Result<ds_ipc::Response, String> {
    let paths =
        ds_config::Paths::resolve().ok_or_else(|| "cannot resolve engine socket".to_string())?;
    ds_ipc::request(&paths.engine_sock, request)
        .map_err(|error| format!("engine unavailable: {error}"))
}

pub(crate) fn run(args: &[String]) -> i32 {
    let result = (|| -> Result<Option<serde_json::Value>, String> {
        let action = args
            .first()
            .map(String::as_str)
            .ok_or_else(|| "expected acquire, ready, renew, or release".to_string())?;
        let options = &args[1..];
        let session_id = option(options, "--session")?;
        let request = match action {
            "acquire" => {
                validate_options(options, &["--id", "--session", "--ttl-ms"])?;
                ds_ipc::Request::AcquireDictationPresenter {
                    presenter_id: option(options, "--id")?,
                    session_id,
                    ttl_ms: ttl(options)?,
                }
            }
            "ready" => {
                validate_options(options, &["--lease", "--session"])?;
                ds_ipc::Request::ReadyDictationPresenter {
                    lease_id: option(options, "--lease")?,
                    session_id,
                }
            }
            "renew" => {
                validate_options(options, &["--lease", "--session", "--ttl-ms"])?;
                ds_ipc::Request::RenewDictationPresenter {
                    lease_id: option(options, "--lease")?,
                    session_id,
                    ttl_ms: ttl(options)?,
                }
            }
            "release" => {
                validate_options(options, &["--lease", "--session"])?;
                ds_ipc::Request::ReleaseDictationPresenter {
                    lease_id: option(options, "--lease")?,
                    session_id,
                }
            }
            _ => return Err(format!("unknown presenter action {action:?}")),
        };
        match response(&request)? {
            ds_ipc::Response::DictationPresenterLease { lease_id, ttl_ms } => {
                if action != "acquire" {
                    return Err("dictation presenter: unexpected lease response".into());
                }
                Ok(Some(serde_json::json!({
                    "lease_id": lease_id,
                    "ttl_ms": ttl_ms,
                })))
            }
            ds_ipc::Response::Done if action != "acquire" => Ok(None),
            ds_ipc::Response::Error { message } => Err(message),
            _ => Err("dictation presenter: unexpected engine response".into()),
        }
    })();
    match result {
        Ok(Some(value)) => match serde_json::to_string(&value) {
            Ok(value) => {
                println!("{value}");
                0
            }
            Err(error) => {
                eprintln!("dontspeak presenter: could not encode response: {error}");
                1
            }
        },
        Ok(None) => 0,
        Err(error) => {
            eprintln!("dontspeak presenter: {error}");
            2
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(args: &[&str]) -> Vec<String> {
        args.iter().map(|arg| (*arg).to_string()).collect()
    }

    #[test]
    fn parses_scoped_lifecycle_options() {
        let acquire = argv(&[
            "--id",
            "herdr.voice",
            "--session",
            "feedface-0000000000000007",
            "--ttl-ms",
            "3500",
        ]);
        validate_options(&acquire, &["--id", "--session", "--ttl-ms"]).unwrap();
        assert_eq!(option(&acquire, "--id").unwrap(), "herdr.voice");
        assert_eq!(
            option(&acquire, "--session").unwrap(),
            "feedface-0000000000000007"
        );
        assert_eq!(ttl(&acquire).unwrap(), 3_500);
    }

    #[test]
    fn rejects_unknown_odd_repeated_and_out_of_range_options() {
        assert!(ttl(&argv(&["--ttl-ms", "499"])).is_err());
        assert!(validate_options(&argv(&["--lease"]), &["--lease"]).is_err());
        assert!(
            validate_options(
                &argv(&["--lease", "token", "--unsafe", "yes"]),
                &["--lease"]
            )
            .is_err()
        );
        assert!(
            option(
                &argv(&["--session", "first", "--session", "second"]),
                "--session"
            )
            .is_err()
        );
    }
}
