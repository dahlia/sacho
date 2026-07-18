use std::collections::HashSet;
use std::ffi::OsString;
use std::path::Path;
use std::process::Command;

use crate::config::VcsPreset;
use url::{Host, Url};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RepositoryUrl {
    pub(crate) web_url: String,
    pub(crate) issue_template: String,
}

pub(crate) fn normalize_explicit(input: &str) -> Option<RepositoryUrl> {
    normalize(input, UrlScope::Explicit)
}

fn normalize_inferred(input: &str) -> Option<RepositoryUrl> {
    normalize(input, UrlScope::Inferred)
}

pub(crate) fn infer(root: &Path, preset: VcsPreset) -> Option<RepositoryUrl> {
    infer_with(root, preset, &ProcessRunner)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UrlScope {
    Explicit,
    Inferred,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RemoteScheme {
    Https,
    Ssh,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Forge {
    Codeberg,
    GitHub,
    GitLab,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ParsedRemote {
    scheme: RemoteScheme,
    host: String,
    display_host: String,
    port: Option<u16>,
    path: String,
}

fn normalize(input: &str, scope: UrlScope) -> Option<RepositoryUrl> {
    let remote = parse_remote(input)?;
    let forge = forge(&remote.host);
    match (scope, remote.scheme, forge) {
        (UrlScope::Inferred, _, None) | (_, RemoteScheme::Ssh, None) => return None,
        _ => {}
    }
    let path = normalize_repository_path(&remote.path)?;
    let port = match remote.scheme {
        RemoteScheme::Https => remote.port.map(|port| format!(":{port}")),
        RemoteScheme::Ssh => None,
    }
    .unwrap_or_default();
    let web_url = format!("https://{}{port}{path}", remote.display_host);
    let issue_path = if forge == Some(Forge::GitLab) {
        "/-/issues/{n}"
    } else {
        "/issues/{n}"
    };
    Some(RepositoryUrl {
        issue_template: format!("{web_url}{issue_path}"),
        web_url,
    })
}

fn parse_remote(input: &str) -> Option<ParsedRemote> {
    let input = input.trim();
    if input.is_empty() {
        return None;
    }
    if input.contains("://") {
        return parse_standard_url(input);
    }
    parse_scp_url(input)
}

fn parse_standard_url(input: &str) -> Option<ParsedRemote> {
    let url = Url::parse(input).ok()?;
    let scheme = match url.scheme() {
        "https" => RemoteScheme::Https,
        "ssh" => RemoteScheme::Ssh,
        _ => return None,
    };
    let host = url.host_str()?.to_ascii_lowercase();
    let display_host = match url.host()? {
        Host::Domain(domain) => domain.to_ascii_lowercase(),
        Host::Ipv4(address) => address.to_string(),
        Host::Ipv6(address) => format!("[{address}]"),
    };
    Some(ParsedRemote {
        scheme,
        host,
        display_host,
        port: url.port(),
        path: url.path().to_owned(),
    })
}

fn parse_scp_url(input: &str) -> Option<ParsedRemote> {
    let (authority, path) = input.split_once(':')?;
    if authority.len() == 1 && authority.as_bytes()[0].is_ascii_alphabetic() {
        return None;
    }
    let host = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    if host
        .chars()
        .any(|character| matches!(character, '/' | '\\'))
    {
        return None;
    }
    let url = format!("ssh://{host}/{}", path.trim_start_matches('/'));
    parse_standard_url(&url)
}

fn normalize_repository_path(path: &str) -> Option<String> {
    let path = path.trim_end_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    let segments = path.strip_prefix('/')?.split('/').collect::<Vec<_>>();
    if segments.len() < 2 || segments.iter().any(|segment| segment.is_empty()) {
        return None;
    }
    Some(format!("/{}", segments.join("/")))
}

fn forge(host: &str) -> Option<Forge> {
    match host {
        "codeberg.org" => Some(Forge::Codeberg),
        "github.com" => Some(Forge::GitHub),
        "gitlab.com" => Some(Forge::GitLab),
        _ => None,
    }
}

trait Runner {
    fn run(&self, root: &Path, program: &str, args: &[OsString]) -> Option<Vec<u8>>;
}

#[derive(Debug, Clone, Copy)]
struct ProcessRunner;

impl Runner for ProcessRunner {
    fn run(&self, root: &Path, program: &str, args: &[OsString]) -> Option<Vec<u8>> {
        let output = Command::new(program)
            .args(args)
            .current_dir(root)
            .output()
            .ok()?;
        output.status.success().then_some(output.stdout)
    }
}

fn infer_with<R: Runner>(root: &Path, preset: VcsPreset, runner: &R) -> Option<RepositoryUrl> {
    match preset {
        VcsPreset::Git => infer_git(root, runner),
        VcsPreset::Jj if root.join(".git").exists() => infer_git(root, runner),
        VcsPreset::Jj => infer_jj(root, runner),
        VcsPreset::Hg => infer_hg(root, runner),
        VcsPreset::None => None,
    }
}

fn infer_git<R: Runner>(root: &Path, runner: &R) -> Option<RepositoryUrl> {
    let remotes = run_lines(runner, root, "git", &["remote"])?;
    let mut candidates = Vec::new();
    if let Some(branch) = run_line(runner, root, "git", &["branch", "--show-current"])
        && let Some(remote) = run_line(
            runner,
            root,
            "git",
            &["config", "--get", &format!("branch.{branch}.remote")],
        )
        && remote != "."
    {
        candidates.push(remote);
    }
    if remotes.iter().any(|remote| remote == "origin") {
        candidates.push(String::from("origin"));
    }
    if remotes.len() == 1 {
        candidates.push(remotes[0].clone());
    }
    let mut seen = HashSet::new();
    for remote in candidates {
        if !seen.insert(remote.clone()) {
            continue;
        }
        let url = run_line_owned(
            runner,
            root,
            "git",
            vec![
                OsString::from("remote"),
                OsString::from("get-url"),
                OsString::from("--"),
                OsString::from(remote),
            ],
        );
        if let Some(url) = url.and_then(|url| normalize_inferred(&url)) {
            return Some(url);
        }
    }
    None
}

fn infer_jj<R: Runner>(root: &Path, runner: &R) -> Option<RepositoryUrl> {
    let output = run_text(
        runner,
        root,
        "jj",
        &[
            "--ignore-working-copy",
            "--no-pager",
            "--color=never",
            "git",
            "remote",
            "list",
        ],
    )?;
    let remotes = parse_jj_remotes(&output)?;
    let candidate = remotes
        .iter()
        .find(|(name, _)| name == "origin")
        .or_else(|| (remotes.len() == 1).then(|| &remotes[0]))?;
    normalize_inferred(&candidate.1)
}

fn parse_jj_remotes(output: &str) -> Option<Vec<(String, String)>> {
    output
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            let (name, value) = line.split_once(' ')?;
            let fetch = value
                .split_once(" (push: ")
                .map_or(value, |(fetch, _)| fetch)
                .trim();
            (!name.is_empty() && !fetch.is_empty()).then(|| (name.to_owned(), fetch.to_owned()))
        })
        .collect()
}

fn infer_hg<R: Runner>(root: &Path, runner: &R) -> Option<RepositoryUrl> {
    for name in ["default", "default-push"] {
        if let Some(url) =
            run_line(runner, root, "hg", &["paths", name]).and_then(|url| normalize_inferred(&url))
        {
            return Some(url);
        }
    }
    None
}

fn run_text<R: Runner>(runner: &R, root: &Path, program: &str, args: &[&str]) -> Option<String> {
    let args = args.iter().map(OsString::from).collect::<Vec<_>>();
    String::from_utf8(runner.run(root, program, &args)?).ok()
}

fn run_lines<R: Runner>(
    runner: &R,
    root: &Path,
    program: &str,
    args: &[&str],
) -> Option<Vec<String>> {
    Some(
        run_text(runner, root, program, args)?
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_owned)
            .collect(),
    )
}

fn run_line<R: Runner>(runner: &R, root: &Path, program: &str, args: &[&str]) -> Option<String> {
    let args = args.iter().map(OsString::from).collect::<Vec<_>>();
    run_line_owned(runner, root, program, args)
}

fn run_line_owned<R: Runner>(
    runner: &R,
    root: &Path,
    program: &str,
    args: Vec<OsString>,
) -> Option<String> {
    let output = String::from_utf8(runner.run(root, program, &args)?).ok()?;
    let mut lines = output
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty());
    let line = lines.next()?.to_owned();
    lines.next().is_none().then_some(line)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::process::Command as ProcessCommand;

    use proptest::prelude::*;

    use super::*;

    #[derive(Debug, Default)]
    struct FakeRunner {
        outputs: HashMap<Vec<String>, Vec<u8>>,
    }

    impl FakeRunner {
        fn with_output<const N: usize>(
            mut self,
            program: &str,
            args: [&str; N],
            output: impl Into<Vec<u8>>,
        ) -> Self {
            self.outputs.insert(
                std::iter::once(program.to_owned())
                    .chain(args.into_iter().map(str::to_owned))
                    .collect(),
                output.into(),
            );
            self
        }
    }

    impl Runner for FakeRunner {
        fn run(&self, _root: &Path, program: &str, args: &[OsString]) -> Option<Vec<u8>> {
            let key = std::iter::once(program.to_owned())
                .chain(
                    args.iter()
                        .map(|argument| argument.to_string_lossy().into_owned()),
                )
                .collect::<Vec<_>>();
            self.outputs.get(&key).cloned()
        }
    }

    #[test]
    fn normalizes_known_forge_clone_urls_and_issue_routes() {
        let cases = [
            (
                "https://codeberg.org/hongminhee/sacho.git/",
                "https://codeberg.org/hongminhee/sacho",
                "https://codeberg.org/hongminhee/sacho/issues/{n}",
            ),
            (
                "ssh://git@github.com:2222/dahlia/sacho.git",
                "https://github.com/dahlia/sacho",
                "https://github.com/dahlia/sacho/issues/{n}",
            ),
            (
                "git@gitlab.com:group/subgroup/project.git",
                "https://gitlab.com/group/subgroup/project",
                "https://gitlab.com/group/subgroup/project/-/issues/{n}",
            ),
        ];

        for (input, web_url, issue_template) in cases {
            assert_eq!(
                normalize_inferred(input),
                Some(RepositoryUrl {
                    web_url: web_url.to_owned(),
                    issue_template: issue_template.to_owned(),
                })
            );
        }
    }

    #[test]
    fn inferred_urls_reject_unknown_hosts_but_explicit_https_accepts_them() {
        let input = "https://forge.example/team/project.git";

        assert_eq!(normalize_inferred(input), None);
        assert_eq!(
            normalize_explicit(input),
            Some(RepositoryUrl {
                web_url: String::from("https://forge.example/team/project"),
                issue_template: String::from("https://forge.example/team/project/issues/{n}"),
            })
        );
    }

    #[test]
    fn normalization_removes_sensitive_url_components() {
        let normalized = normalize_inferred(
            "https://user:password@codeberg.org/team/project.git?token=secret#branch",
        )
        .expect("sanitized URL");

        assert_eq!(normalized.web_url, "https://codeberg.org/team/project");
        assert!(!normalized.issue_template.contains("user"));
        assert!(!normalized.issue_template.contains("password"));
        assert!(!normalized.issue_template.contains("secret"));
        assert!(!normalized.issue_template.contains("branch"));
    }

    #[test]
    fn https_ports_are_preserved_while_ssh_ports_are_removed() {
        assert_eq!(
            normalize_inferred("https://codeberg.org:8443/team/project.git")
                .expect("HTTPS URL")
                .web_url,
            "https://codeberg.org:8443/team/project"
        );
        assert_eq!(
            normalize_inferred("ssh://git@codeberg.org:443/team/project.git")
                .expect("SSH URL")
                .web_url,
            "https://codeberg.org/team/project"
        );
    }

    #[test]
    fn rejects_unsafe_or_non_repository_urls() {
        for input in [
            "http://github.com/team/project",
            "git://github.com/team/project",
            "file:///tmp/project",
            "/tmp/project",
            "../project",
            "not a URL",
            "https://github.com/project",
            "ssh://github.com/project",
            "git@github.com/team:project/repository",
            "git@github.com\\team:project/repository",
            "https://github.com/team//project",
        ] {
            assert_eq!(normalize_explicit(input), None, "accepted {input:?}");
            assert_eq!(normalize_inferred(input), None, "inferred {input:?}");
        }
    }

    #[test]
    fn git_prefers_the_current_branch_remote_over_origin() {
        let runner = FakeRunner::default()
            .with_output("git", ["remote"], "origin\nupstream\n")
            .with_output("git", ["branch", "--show-current"], "feature\n")
            .with_output(
                "git",
                ["config", "--get", "branch.feature.remote"],
                "upstream\n",
            )
            .with_output(
                "git",
                ["remote", "get-url", "--", "upstream"],
                "ssh://git@codeberg.org/team/project.git\n",
            )
            .with_output(
                "git",
                ["remote", "get-url", "--", "origin"],
                "https://github.com/team/fork.git\n",
            );

        assert_eq!(
            infer_git(Path::new("."), &runner)
                .expect("tracked remote")
                .web_url,
            "https://codeberg.org/team/project"
        );
    }

    #[test]
    fn git_falls_back_to_origin_then_a_sole_remote() {
        let origin = FakeRunner::default()
            .with_output("git", ["remote"], "origin\nupstream\n")
            .with_output("git", ["branch", "--show-current"], "feature\n")
            .with_output("git", ["config", "--get", "branch.feature.remote"], ".\n")
            .with_output(
                "git",
                ["remote", "get-url", "--", "origin"],
                "https://github.com/team/project.git\n",
            );
        let sole = FakeRunner::default()
            .with_output("git", ["remote"], "codeberg\n")
            .with_output(
                "git",
                ["remote", "get-url", "--", "codeberg"],
                "git@codeberg.org:team/project.git\n",
            );

        assert_eq!(
            infer_git(Path::new("."), &origin).expect("origin").web_url,
            "https://github.com/team/project"
        );
        assert_eq!(
            infer_git(Path::new("."), &sole)
                .expect("sole remote")
                .web_url,
            "https://codeberg.org/team/project"
        );
    }

    #[test]
    fn git_rejects_ambiguous_or_malformed_remote_output() {
        let ambiguous = FakeRunner::default()
            .with_output("git", ["remote"], "one\ntwo\n")
            .with_output("git", ["branch", "--show-current"], "\n");
        let malformed = FakeRunner::default()
            .with_output("git", ["remote"], "origin\n")
            .with_output("git", ["branch", "--show-current"], "main\n")
            .with_output("git", ["config", "--get", "branch.main.remote"], "origin\n")
            .with_output(
                "git",
                ["remote", "get-url", "--", "origin"],
                b"https://github.com/team/project.git\nextra\n".to_vec(),
            );
        let no_origin = FakeRunner::default()
            .with_output("git", ["remote"], "one\ntwo\n")
            .with_output("git", ["branch", "--show-current"], "\n")
            .with_output(
                "git",
                ["remote", "get-url", "--", "origin"],
                "https://github.com/team/project.git\n",
            );

        assert_eq!(infer_git(Path::new("."), &ambiguous), None);
        assert_eq!(infer_git(Path::new("."), &malformed), None);
        assert_eq!(infer_git(Path::new("."), &no_origin), None);
    }

    #[test]
    fn jujutsu_dispatches_native_and_colocated_repositories_differently() {
        let native = tempfile::TempDir::new().expect("native Jujutsu tempdir");
        let native_runner = FakeRunner::default().with_output(
            "jj",
            [
                "--ignore-working-copy",
                "--no-pager",
                "--color=never",
                "git",
                "remote",
                "list",
            ],
            "origin https://github.com/team/native.git\n",
        );
        assert_eq!(
            infer_with(native.path(), VcsPreset::Jj, &native_runner)
                .expect("native Jujutsu remote")
                .web_url,
            "https://github.com/team/native"
        );

        let colocated = tempfile::TempDir::new().expect("colocated Jujutsu tempdir");
        std::fs::create_dir(colocated.path().join(".git")).expect("Git marker");
        let colocated_runner = FakeRunner::default()
            .with_output("git", ["remote"], "origin\n")
            .with_output("git", ["branch", "--show-current"], "\n")
            .with_output(
                "git",
                ["remote", "get-url", "--", "origin"],
                "https://codeberg.org/team/colocated.git\n",
            );
        assert_eq!(
            infer_with(colocated.path(), VcsPreset::Jj, &colocated_runner)
                .expect("colocated Jujutsu remote")
                .web_url,
            "https://codeberg.org/team/colocated"
        );
    }

    #[test]
    fn jujutsu_prefers_origin_and_ignores_its_push_url() {
        let runner = FakeRunner::default().with_output(
            "jj",
            [
                "--ignore-working-copy",
                "--no-pager",
                "--color=never",
                "git",
                "remote",
                "list",
            ],
            "origin https://github.com/team/project.git (push: ssh://token@github.com/team/fork.git)\nupstream https://codeberg.org/team/project.git\n",
        );

        assert_eq!(
            infer_jj(Path::new("."), &runner).expect("origin").web_url,
            "https://github.com/team/project"
        );
    }

    #[test]
    fn jujutsu_uses_a_sole_remote_but_rejects_ambiguous_remotes() {
        let sole = FakeRunner::default().with_output(
            "jj",
            [
                "--ignore-working-copy",
                "--no-pager",
                "--color=never",
                "git",
                "remote",
                "list",
            ],
            "upstream ssh://git@codeberg.org/team/project.git\n",
        );
        let ambiguous = FakeRunner::default().with_output(
            "jj",
            [
                "--ignore-working-copy",
                "--no-pager",
                "--color=never",
                "git",
                "remote",
                "list",
            ],
            "one https://github.com/team/one.git\ntwo https://github.com/team/two.git\n",
        );
        let unnamed = FakeRunner::default().with_output(
            "jj",
            [
                "--ignore-working-copy",
                "--no-pager",
                "--color=never",
                "git",
                "remote",
                "list",
            ],
            " https://github.com/team/project.git\n",
        );

        assert_eq!(
            infer_jj(Path::new("."), &sole)
                .expect("sole remote")
                .web_url,
            "https://codeberg.org/team/project"
        );
        assert_eq!(infer_jj(Path::new("."), &ambiguous), None);
        assert_eq!(infer_jj(Path::new("."), &unnamed), None);
    }

    #[test]
    fn mercurial_prefers_default_and_falls_back_to_default_push() {
        let preferred = FakeRunner::default()
            .with_output(
                "hg",
                ["paths", "default"],
                "https://codeberg.org/team/project.git\n",
            )
            .with_output(
                "hg",
                ["paths", "default-push"],
                "https://github.com/team/fork.git\n",
            );
        let fallback = FakeRunner::default()
            .with_output("hg", ["paths", "default"], "/local/project\n")
            .with_output(
                "hg",
                ["paths", "default-push"],
                "ssh://git@gitlab.com/group/project.git\n",
            );

        assert_eq!(
            infer_hg(Path::new("."), &preferred)
                .expect("default")
                .web_url,
            "https://codeberg.org/team/project"
        );
        assert_eq!(
            infer_hg(Path::new("."), &fallback)
                .expect("default-push")
                .web_url,
            "https://gitlab.com/group/project"
        );
    }

    #[test]
    fn inference_silently_rejects_non_utf8_command_output() {
        let runner = FakeRunner::default().with_output("git", ["remote"], vec![0xff]);

        assert_eq!(infer_git(Path::new("."), &runner), None);
    }

    #[test]
    #[ignore = "requires Git, Jujutsu, and Mercurial executables"]
    fn repository_url_live_vcs_smoke_test() {
        let git = tempfile::TempDir::new().expect("Git tempdir");
        process(git.path(), "git", ["init"]);
        process(
            git.path(),
            "git",
            [
                "remote",
                "add",
                "origin",
                "ssh://git@codeberg.org/team/project.git",
            ],
        );
        assert_eq!(
            infer(git.path(), VcsPreset::Git)
                .expect("Git remote")
                .web_url,
            "https://codeberg.org/team/project"
        );

        let jj = tempfile::TempDir::new().expect("Jujutsu tempdir");
        process(jj.path(), "jj", ["git", "init", "--no-colocate"]);
        process(
            jj.path(),
            "jj",
            [
                "git",
                "remote",
                "add",
                "origin",
                "https://github.com/team/project.git",
            ],
        );
        assert_eq!(
            infer(jj.path(), VcsPreset::Jj)
                .expect("Jujutsu remote")
                .web_url,
            "https://github.com/team/project"
        );

        let colocated_jj = tempfile::TempDir::new().expect("colocated Jujutsu tempdir");
        process(colocated_jj.path(), "git", ["init"]);
        process(
            colocated_jj.path(),
            "git",
            [
                "remote",
                "add",
                "origin",
                "ssh://git@gitlab.com/group/project.git",
            ],
        );
        process(colocated_jj.path(), "jj", ["git", "init", "--colocate"]);
        assert_eq!(
            infer(colocated_jj.path(), VcsPreset::Jj)
                .expect("colocated Jujutsu remote")
                .web_url,
            "https://gitlab.com/group/project"
        );

        let hg = tempfile::TempDir::new().expect("Mercurial tempdir");
        process(hg.path(), "hg", ["init"]);
        std::fs::write(
            hg.path().join(".hg/hgrc"),
            "[paths]\ndefault = git@gitlab.com:group/project.git\n",
        )
        .expect("Mercurial configuration");
        assert_eq!(
            infer(hg.path(), VcsPreset::Hg)
                .expect("Mercurial remote")
                .web_url,
            "https://gitlab.com/group/project"
        );
    }

    fn process<const N: usize>(root: &Path, program: &str, args: [&str; N]) {
        let output = ProcessCommand::new(program)
            .args(args)
            .current_dir(root)
            .output()
            .expect("VCS command");
        assert!(
            output.status.success(),
            "{program} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    proptest! {
        #[test]
        fn normalization_is_stable_across_git_suffixes_and_trailing_slashes(
            owner in "[A-Za-z0-9_-]{1,20}",
            repository in "[A-Za-z0-9_-]{1,20}",
            slashes in "/{0,4}",
        ) {
            let plain = format!("https://github.com/{owner}/{repository}");
            let decorated = format!("{plain}.git{slashes}");

            prop_assert_eq!(normalize_inferred(&plain), normalize_inferred(&decorated));
        }
    }
}
