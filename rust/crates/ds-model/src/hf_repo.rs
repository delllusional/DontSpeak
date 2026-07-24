//! Self-managed Hugging Face model sets: the shape, the roots, and the transfer.
//!
//! We fetch every self-managed HF asset with the same HTTP/retry/SHA/atomic-rename/progress
//! path as ONNX. The native shims load only these local directories. Each set pins an
//! immutable HF commit and every selected file's path, size, and SHA-256 in this repository.
//! `.ds-ready` records the revision after all pinned bytes verify (status polling stays
//! network-free). The manifests live beside their runtime ([`crate::mlx_repo`]); this module
//! owns only the machinery they share.

use std::path::{Path, PathBuf};

use crate::download::{DEFAULT_RETRIES, ensure_at};
use crate::hash::verify_sha256_cached;
use crate::spec::ModelSpec;

pub(crate) const HF_HOST: &str = "https://huggingface.co";
/// Written into a model dir once every file is present + verified; holds the pinned revision
/// so bumping the pin invalidates a stale tree and forces a re-fetch.
pub(crate) const READY_MARKER: &str = ".ds-ready";

/// Which on-disk root a repo's `dir_name` hangs under.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepoRoot {
    /// `<model>/mlx/<dir_name>` — the MLX Audio sets.
    Mlx,
    /// `<model>/coreml/<dir_name>` — Core ML sets DontSpeak hands an explicit directory to.
    CoreMl,
    /// `<fluid>/<dir_name>` — FluidAudio's OWN cache. Its G2P singleton hardcodes
    /// `TtsCacheDirectory.ensure()/Models/kokoro`: no env var, no settable property, and the
    /// Swift side derives the home directory from `getpwuid`, so no `HOME` override can
    /// redirect it. DontSpeak pre-fills that exact directory instead.
    FluidCache,
}

/// Every on-disk root an asset can live under, as VALUES rather than ambient lookups.
///
/// One resolution point ([`ModelRoots::ambient`]) at the process boundary; everything below
/// it takes a `&ModelRoots`. That is what lets a removal reclaim a third-party cache while a
/// test builds every root inside one tempdir.
#[derive(Debug, Clone)]
pub struct ModelRoots {
    /// DontSpeak's model cache ([`ds_config::model_dir`]).
    pub model: PathBuf,
    /// FluidAudio's TTS cache ([`ds_config::fluidaudio_models_dir`]).
    pub fluid: PathBuf,
}

impl ModelRoots {
    /// The ONE ambient resolution. Process boundary only. `None` exactly when
    /// [`ds_config::model_dir`] is `None` — the fluid root is platform-neutral path math.
    pub fn ambient() -> Option<Self> {
        let roots = Self {
            model: ds_config::model_dir()?,
            fluid: ds_config::fluidaudio_models_dir()?,
        };
        // Nesting would make the recursive orphan sweep of one root walk the other; the
        // containment arm in `download::orphan_sweep_root` tests `fluid` first so a nested
        // pair still confines, but the layout itself is a configuration mistake.
        debug_assert!(
            !roots.fluid.starts_with(&roots.model) && !roots.model.starts_with(&roots.fluid),
            "model root {} and fluid root {} must be disjoint",
            roots.model.display(),
            roots.fluid.display()
        );
        Some(roots)
    }

    /// Every root inside `dir`. Test seam, and the only other constructor. Pure path math:
    /// creates nothing, so a fixture can assert its root was never materialized.
    pub fn under(dir: &Path) -> Self {
        Self {
            model: dir.join("models"),
            fluid: dir.join("fluidaudio"),
        }
    }

    /// Total — a repo always has a directory once the roots resolve.
    pub fn dir_for(&self, repo: &HfRepo) -> PathBuf {
        match repo.root {
            RepoRoot::Mlx => ds_config::mlx_dir_under(&self.model).join(repo.dir_name),
            RepoRoot::CoreMl => ds_config::coreml_dir_under(&self.model).join(repo.dir_name),
            RepoRoot::FluidCache => self.fluid.join(repo.dir_name),
        }
    }
}

/// Ambient form of [`ModelRoots::dir_for`]. Derived, never an independent resolution.
pub fn repo_dir(repo: &HfRepo) -> Option<PathBuf> {
    Some(ModelRoots::ambient()?.dir_for(repo))
}

/// One source-pinned file in a Hugging Face repository.
#[derive(Debug, Clone, Copy)]
pub struct HfFile {
    pub path: &'static str,
    pub size: u64,
    pub sha256: &'static str,
}

/// One model set, pinned to an immutable HF revision and static file manifest.
pub struct HfRepo {
    pub name: &'static str,
    pub repo: &'static str,
    pub revision: &'static str,
    pub files: &'static [HfFile],
    /// Directory name under the root `root` names. [`ModelRoots::dir_for`] is the ONE
    /// resolution of the pair, so the downloader, the presence probe and the removal
    /// cannot land on different directories.
    pub dir_name: &'static str,
    pub root: RepoRoot,
    /// Library-catalog metadata (the Apple-Silicon model sets shown in the Libraries tab).
    /// The license lives WITH the files here — same can't-drift principle as
    /// [`crate::urls::Project`] — so `crate::libraries` can render these alongside the
    /// downloaded ONNX assets without a second, drift-prone source. `display_name`/`usage`
    /// empty ⇒ the set is an internal sub-component of another entry (e.g. the Kokoro G2P
    /// sub-models share the Kokoro repo) and is folded into it, not listed on its own.
    pub display_name: &'static str,
    pub usage: &'static str,
    pub license: &'static str,
    pub license_url: &'static str,
}

/// LOCAL presence of a whole set (no network): every repo's completion marker present at the
/// pinned revision — see [`is_hf_repo_present`].
pub fn is_hf_set_present(roots: &ModelRoots, set: &[&HfRepo]) -> bool {
    set.iter().all(|r| is_hf_repo_present(roots, r))
}

/// Completion marker in `dir` carries `repo`'s pinned revision. Existence-only half of
/// [`is_hf_repo_present`], for the inventory probe.
pub(crate) fn ready_marker_matches(dir: &Path, repo: &HfRepo) -> bool {
    std::fs::read_to_string(dir.join(READY_MARKER))
        .is_ok_and(|marker| marker.trim() == repo.revision)
}

/// LOCAL presence (no network): marker revision matches and every source-pinned file verifies.
pub fn is_hf_repo_present(roots: &ModelRoots, repo: &HfRepo) -> bool {
    let target = roots.dir_for(repo);
    ready_marker_matches(&target, repo)
        && !repo.files.is_empty()
        && repo.files.iter().all(|file| {
            let relative = Path::new(file.path);
            relative
                .components()
                .all(|part| matches!(part, std::path::Component::Normal(_)))
                && verify_sha256_cached(&target.join(relative), file.sha256)
        })
}

fn already_have(dest: &Path, file: &HfFile) -> bool {
    verify_sha256_cached(dest, file.sha256)
}

fn download_one_at(
    url: &str,
    file: &HfFile,
    dest: &Path,
    progress: &dyn Fn(u64, u64),
) -> std::io::Result<()> {
    let spec = ModelSpec {
        file_name: file.path.to_string(),
        url: url.to_string(),
        sha256: file.sha256.to_string(),
    };
    ensure_at(dest, &spec, DEFAULT_RETRIES, progress)
}

/// Download a SET of repos as one unit, reporting ONE overall byte-weighted bar:
/// `progress(done_bytes, total_bytes)` where BOTH are summed across every file of every repo —
/// so the UI shows a single monotonic "Downloading `<pct>`%" over the WHOLE set (a true global
/// percent, not a per-file percent that resets each file). Writes each repo's completion marker
/// once its own files verify — per repo, so a failed member keeps completed repos' markers.
/// Missing files fetch concurrently (bounded pool).
pub fn ensure_hf_repos(
    roots: &ModelRoots,
    repos: &[&HfRepo],
    progress: &dyn Fn(u64, u64),
) -> std::io::Result<()> {
    ensure_hf_repos_at(roots, HF_HOST, repos, progress)
}

/// Same as [`ensure_hf_repos`] but resolve URLs are rooted at `host` (production:
/// [`HF_HOST`]; tests: httpmock base URL).
pub(crate) fn ensure_hf_repos_at(
    roots: &ModelRoots,
    host: &str,
    repos: &[&HfRepo],
    progress: &dyn Fn(u64, u64),
) -> std::io::Result<()> {
    let mut plan: Vec<(&HfRepo, PathBuf)> = Vec::new();
    for r in repos {
        if is_hf_repo_present(roots, r) {
            continue;
        }
        if r.files.is_empty() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("manifest for {} is empty", r.name),
            ));
        }
        plan.push((r, roots.dir_for(r)));
    }
    let total_bytes: u64 = plan
        .iter()
        .flat_map(|(repo, _)| repo.files)
        .map(|file| file.size)
        .sum();
    if total_bytes == 0 {
        progress(1, 1);
        return Ok(());
    }

    // One directory flight per planned repo dir for the whole run — the granularity a
    // `models remove` deletes at, so the two exclude each other instead of interleaving
    // per file. The pre-check above still runs outside the flight: a removal landing in
    // that window makes a concurrent cross-process `ds-helper --prefetch` skip a repo it
    // believed present, which the engine's next `compute_needs` probe re-fetches.
    let dirs: Vec<PathBuf> = plan.iter().map(|(_, target)| target.clone()).collect();
    crate::download::with_destination_flights_in(Some(roots), &dirs, || {
        ensure_hf_repos_locked(host, &plan, total_bytes, progress)
    })
}

fn ensure_hf_repos_locked(
    host: &str,
    plan: &[(&HfRepo, PathBuf)],
    total_bytes: u64,
    progress: &dyn Fn(u64, u64),
) -> std::io::Result<()> {
    let mut pre_credit: u64 = 0;
    let mut jobs: Vec<crate::parallel::DownloadJob> = Vec::new();
    for (repo, target) in plan {
        for file in repo.files {
            let relative = Path::new(file.path);
            if relative
                .components()
                .any(|c| !matches!(c, std::path::Component::Normal(_)))
            {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("unsafe path in manifest: {}", file.path),
                ));
            }
            let dest = target.join(relative);
            let size = file.size;
            if already_have(&dest, file) {
                pre_credit = pre_credit.saturating_add(size);
                continue;
            }
            let host = host.to_string();
            let repo_id = repo.repo;
            let revision = repo.revision;
            let file = *file;
            jobs.push(Box::new(move |p| {
                // Local progress is file bytes; pool sums high-waters + pre_credit.
                let emit = |done: u64, _t: u64| p(done.min(size), size);
                let url = format!("{host}/{repo_id}/resolve/{revision}/{}", file.path);
                download_one_at(&url, &file, &dest, &emit)
            }));
        }
    }

    let pool_result = crate::parallel::run_jobs_parallel(
        progress,
        total_bytes,
        pre_credit.min(total_bytes),
        jobs,
    );

    // Per-repo marker atomicity: write each repo's marker as soon as ALL of its own files verify,
    // independent of sibling repos in the same set. A failed sibling no longer discards a completed
    // repo's marker, so the next retry skips the finished repo. A repo with any invalid file gets
    // no marker and self-repairs on the next run.
    for (repo, target) in plan {
        if let Some(missing) = repo
            .files
            .iter()
            .find(|file| !already_have(&target.join(Path::new(file.path)), file))
        {
            // After a *successful* pool run every file must be present; a gap is a real bug, not
            // the partial-failure case (which already carries the pool's own error).
            if pool_result.is_ok() {
                return Err(std::io::Error::other(format!(
                    "missing verified file after download: {}",
                    missing.path
                )));
            }
            continue;
        }
        std::fs::write(target.join(READY_MARKER), repo.revision)?;
    }

    pool_result
}

#[cfg(test)]
pub(crate) fn fixture_file(path: &'static str, content: &'static [u8]) -> HfFile {
    HfFile {
        path,
        size: content.len() as u64,
        sha256: Box::leak(crate::hash::sha256_hex(content).into_boxed_str()),
    }
}

#[cfg(test)]
pub(crate) fn fixture_files(files: Vec<HfFile>) -> &'static [HfFile] {
    Box::leak(files.into_boxed_slice())
}

/// A manifest-shaped repo whose `dir_name` is its own name, so two fixtures in one set land
/// in distinct directories under the same roots.
#[cfg(test)]
pub(crate) fn fixture_repo(
    name: &'static str,
    repo: &'static str,
    revision: &'static str,
    files: &'static [HfFile],
    root: RepoRoot,
) -> HfRepo {
    HfRepo {
        name,
        repo,
        revision,
        files,
        dir_name: name,
        root,
        display_name: "",
        usage: "",
        license: "",
        license_url: "",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pure path math (no FS): one resolution per repo, under the root it declares. There is
    /// only one spelling of that resolution now, so what is left to pin is the LAYOUT each
    /// [`RepoRoot`] promises.
    #[test]
    fn every_repo_resolves_under_its_declared_root() {
        let dir = Path::new("/roots");
        let roots = ModelRoots::under(dir);
        let files = fixture_files(vec![fixture_file("weights.bin", b"bytes")]);

        for repo in crate::mlx_repo::all_mlx_repos() {
            assert_eq!(repo.root, RepoRoot::Mlx, "{}", repo.name);
            assert!(!repo.dir_name.is_empty(), "{}", repo.name);
            assert_eq!(
                roots.dir_for(repo),
                roots.model.join("mlx").join(repo.dir_name),
                "{}",
                repo.name
            );
        }

        let coreml = fixture_repo("coreml", "test-org/coreml", "rev", files, RepoRoot::CoreMl);
        assert_eq!(
            roots.dir_for(&coreml),
            roots.model.join("coreml").join("coreml")
        );

        // A third-party cache is NOT under the model root: that is the whole point of the
        // second root, and what makes a removal there something to bound (#212).
        let fluid = fixture_repo(
            "kokoro",
            "test-org/fluid",
            "rev",
            files,
            RepoRoot::FluidCache,
        );
        let fluid_dir = roots.dir_for(&fluid);
        assert_eq!(fluid_dir, roots.fluid.join("kokoro"));
        assert!(!fluid_dir.starts_with(&roots.model));
    }

    /// `under` yields both roots on any host, and `ambient` resolves exactly when the model
    /// dir does — the fluid root is platform-neutral path math, not a macOS-gated lookup, so
    /// `dir_for` never has to be fallible.
    #[test]
    fn model_roots_resolve_on_every_platform() {
        let tmp = tempfile::tempdir().unwrap();
        let roots = ModelRoots::under(tmp.path());
        assert_eq!(roots.model, tmp.path().join("models"));
        assert_eq!(roots.fluid, tmp.path().join("fluidaudio"));
        assert!(!roots.model.exists(), "`under` creates nothing");
        assert!(!roots.fluid.exists());

        assert_eq!(
            ModelRoots::ambient().is_some(),
            ds_config::model_dir().is_some()
        );
        assert!(ds_config::fluidaudio_models_dir().is_some());
    }

    #[test]
    fn presence_requires_matching_marker_and_every_pinned_digest() {
        const CONTENT: &[u8] = b"verified weights";
        let tmp = tempfile::tempdir().unwrap();
        let roots = ModelRoots::under(tmp.path());
        let files = fixture_files(vec![fixture_file("model.safetensors", CONTENT)]);
        let repo = fixture_repo(
            "presence",
            "test-org/presence",
            "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef",
            files,
            RepoRoot::Mlx,
        );
        let dir = roots.dir_for(&repo);
        std::fs::create_dir_all(&dir).unwrap();

        assert!(!is_hf_repo_present(&roots, &repo));
        std::fs::write(dir.join(READY_MARKER), "different-revision").unwrap();
        assert!(!is_hf_repo_present(&roots, &repo));

        std::fs::write(dir.join(READY_MARKER), repo.revision).unwrap();
        assert!(
            !is_hf_repo_present(&roots, &repo),
            "pinned file is still missing"
        );

        let model = dir.join("model.safetensors");
        std::fs::write(&model, CONTENT).unwrap();
        assert!(is_hf_repo_present(&roots, &repo));

        // Different length so file identity (size + mtime) invalidates the SHA
        // cache even on filesystems with coarse mtime resolution.
        std::fs::write(&model, b"tampered").unwrap();
        assert!(
            !is_hf_repo_present(&roots, &repo),
            "matching marker cannot bless changed bytes"
        );
    }

    #[test]
    fn download_one_at_persists_only_bytes_matching_the_static_pin() {
        const CONTENT: &[u8] = b"fake model weights";
        let server = httpmock::MockServer::start();
        let good = server.mock(|when, then| {
            when.method(httpmock::Method::GET).path("/good.bin");
            then.status(200).body(CONTENT);
        });
        let bad = server.mock(|when, then| {
            when.method(httpmock::Method::GET).path("/bad.bin");
            then.status(200).body(b"tampered bytes");
        });

        let dir = tempfile::tempdir().unwrap();
        let file = fixture_file("model.bin", CONTENT);
        let good_dest = dir.path().join("good.bin");
        download_one_at(&server.url("/good.bin"), &file, &good_dest, &|_, _| {})
            .expect("matching bytes land");
        good.assert();
        assert_eq!(std::fs::read(&good_dest).unwrap(), CONTENT);

        let bad_dest = dir.path().join("bad.bin");
        let error =
            download_one_at(&server.url("/bad.bin"), &file, &bad_dest, &|_, _| {}).unwrap_err();
        bad.assert();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert!(!bad_dest.exists());
    }

    #[test]
    fn orchestrator_uses_static_paths_precredits_and_writes_marker() {
        const A: &[u8] = b"already present";
        const B: &[u8] = b"download me";
        const REV: &str = "abc1230000000000000000000000000000000000";
        const REPO_ID: &str = "test-org/static-model";
        let server = httpmock::MockServer::start();
        let blob_a = server.mock(|when, then| {
            when.method(httpmock::Method::GET)
                .path(format!("/{REPO_ID}/resolve/{REV}/a.bin"));
            then.status(200).body(A);
        });
        let blob_b = server.mock(|when, then| {
            when.method(httpmock::Method::GET)
                .path(format!("/{REPO_ID}/resolve/{REV}/nested/b.bin"));
            then.status(200).body(B);
        });

        let tmp = tempfile::tempdir().unwrap();
        let roots = ModelRoots::under(tmp.path());
        let files = fixture_files(vec![
            fixture_file("a.bin", A),
            fixture_file("nested/b.bin", B),
        ]);
        let repo = fixture_repo("static", REPO_ID, REV, files, RepoRoot::Mlx);
        let dir = roots.dir_for(&repo);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.bin"), A).unwrap();
        let progress = std::sync::Mutex::new(Vec::new());

        ensure_hf_repos_at(&roots, &server.base_url(), &[&repo], &|done, total| {
            assert_eq!(total, (A.len() + B.len()) as u64);
            progress.lock().unwrap().push(done);
        })
        .expect("static manifest downloads");

        assert_eq!(blob_a.calls(), 0, "verified file is precredited");
        blob_b.assert();
        assert_eq!(std::fs::read(dir.join("nested/b.bin")).unwrap(), B);
        assert_eq!(
            std::fs::read_to_string(dir.join(READY_MARKER)).unwrap(),
            REV
        );
        assert!(is_hf_repo_present(&roots, &repo));
        let progress = progress.lock().unwrap();
        assert!(progress.windows(2).all(|values| values[1] >= values[0]));
        assert_eq!(*progress.last().unwrap(), (A.len() + B.len()) as u64);
    }

    #[test]
    fn failed_file_does_not_write_a_ready_marker() {
        const GOOD: &[u8] = b"good bytes";
        const BAD_EXPECTED: &[u8] = b"expected bytes";
        const REV: &str = "abc1240000000000000000000000000000000000";
        const REPO_ID: &str = "test-org/failing-model";
        let server = httpmock::MockServer::start();
        let _good = server.mock(|when, then| {
            when.method(httpmock::Method::GET)
                .path(format!("/{REPO_ID}/resolve/{REV}/good.bin"));
            then.status(200).body(GOOD);
        });
        let bad = server.mock(|when, then| {
            when.method(httpmock::Method::GET)
                .path(format!("/{REPO_ID}/resolve/{REV}/bad.bin"));
            then.status(200).body(b"wrong bytes");
        });

        let tmp = tempfile::tempdir().unwrap();
        let roots = ModelRoots::under(tmp.path());
        let files = fixture_files(vec![
            fixture_file("good.bin", GOOD),
            fixture_file("bad.bin", BAD_EXPECTED),
        ]);
        let repo = fixture_repo("failure", REPO_ID, REV, files, RepoRoot::Mlx);

        let error =
            ensure_hf_repos_at(&roots, &server.base_url(), &[&repo], &|_, _| {}).unwrap_err();
        bad.assert();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert!(!roots.dir_for(&repo).join(READY_MARKER).exists());
    }

    #[test]
    fn completed_repo_keeps_its_marker_when_a_sibling_fails() {
        const GOOD: &[u8] = b"good repository";
        const BAD_EXPECTED: &[u8] = b"expected sibling";
        const GOOD_REV: &str = "abc1250000000000000000000000000000000000";
        const BAD_REV: &str = "abc1260000000000000000000000000000000000";
        const GOOD_ID: &str = "test-org/good";
        const BAD_ID: &str = "test-org/bad";
        let server = httpmock::MockServer::start();
        let good_blob = server.mock(|when, then| {
            when.method(httpmock::Method::GET)
                .path(format!("/{GOOD_ID}/resolve/{GOOD_REV}/good.bin"));
            then.status(200).body(GOOD);
        });
        let bad_blob = server.mock(|when, then| {
            when.method(httpmock::Method::GET)
                .path(format!("/{BAD_ID}/resolve/{BAD_REV}/bad.bin"));
            then.status(200).body(b"wrong sibling");
        });

        let tmp = tempfile::tempdir().unwrap();
        let roots = ModelRoots::under(tmp.path());
        let good_files = fixture_files(vec![fixture_file("good.bin", GOOD)]);
        let bad_files = fixture_files(vec![fixture_file("bad.bin", BAD_EXPECTED)]);
        let good_repo = fixture_repo("good", GOOD_ID, GOOD_REV, good_files, RepoRoot::Mlx);
        let bad_repo = fixture_repo("bad", BAD_ID, BAD_REV, bad_files, RepoRoot::Mlx);

        let error = ensure_hf_repos_at(
            &roots,
            &server.base_url(),
            &[&good_repo, &bad_repo],
            &|_, _| {},
        )
        .unwrap_err();

        good_blob.assert();
        bad_blob.assert();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert_eq!(
            std::fs::read_to_string(roots.dir_for(&good_repo).join(READY_MARKER)).unwrap(),
            GOOD_REV
        );
        assert!(!roots.dir_for(&bad_repo).join(READY_MARKER).exists());
    }
}
