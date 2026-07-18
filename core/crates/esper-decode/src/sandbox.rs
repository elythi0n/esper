//! The jail the decoder runs in.
//!
//! The decoder is a large C parser eating bytes a peer chose. The rest of the
//! project is written to be correct; this is written on the assumption that
//! the decoder is not, and that what matters is how little a compromise can
//! reach.
//!
//! # Four layers, chosen because they fail differently
//!
//! Defence in depth is only depth if the layers do not share a failure mode.
//! These do not:
//!
//! | Layer | Stops | Fails if |
//! |---|---|---|
//! | **Address space limit** | the decoder allocating for a picture a peer declared | the kernel ignores `RLIMIT_AS`, which it does not |
//! | **Landlock** | reaching any file, by any path | the kernel is older than 5.13, or Landlock is not enabled |
//! | **seccomp-bpf** | every syscall not on a short list — including all networking | the filter has a gap |
//! | **A separate process** | a compromise reaching the session keys or the input injector | the parent trusts what the child sends it |
//!
//! The last row is the one people forget. A sandbox contains the compromise;
//! it does not make the compromised thing's output trustworthy. The parent
//! parses the child's messages, so **that parser is also a trust boundary** —
//! see [`crate::protocol`], where every length is bounded before it is used.
//!
//! # The ordering is the security property
//!
//! Setup needs syscalls the sandbox forbids: creating the decoder, allocating
//! the first buffers. So the sandbox goes on *after* setup and *before* the
//! first byte from a peer is read. Getting that order wrong produces a
//! decoder that works perfectly and is not sandboxed at all, with nothing to
//! show it.
//!
//! So the child announces itself ready only after [`Sandbox::apply`] has
//! returned, the parent refuses to send anything before that announcement, and
//! there is a test that the announcement carries a sandbox which is actually
//! in force rather than a claim that it is.
//!
//! # Why the default is to kill
//!
//! An unexpected syscall kills the process rather than returning `EPERM`. A
//! decoder handed `EPERM` from something it did not expect to fail behaves
//! unpredictably, and unpredictable is the state an attacker is trying to
//! reach. Dying is unambiguous: the parent sees the child gone and drops the
//! session, which is the correct outcome for a stream that has just tried to
//! do something a decoder has no business doing.
//!
//! The cost is that a legitimate syscall missing from the filter is a crash on
//! somebody's unfamiliar libc. `ESPER_SANDBOX_AUDIT=1` turns the filter into
//! log-and-allow so the missing syscall can be found rather than guessed at.
//! **Audit mode is not a sandbox** and [`SandboxReport::is_enforcing`] says so.

/// Bytes of address space the decoder may map.
///
/// **This is the bound that the in-process decoder cannot have.** openh264
/// allocates for whatever picture size a sequence parameter set declares, and
/// it does so inside `decode`, before returning anything our code could
/// inspect. A peer declaring 16384 by 16384 gets an allocation attempt, and
/// refusing the result afterwards is too late.
///
/// 768 MB: comfortably above an 8K decoder's working set, and far below the
/// point where a peer can make a viewer's machine swap. Past it, `mmap`
/// returns null, openh264's allocation fails, and the child exits — which the
/// parent sees as a dropped stream rather than a wedged machine.
pub const MAX_ADDRESS_SPACE: u64 = 768 * 1024 * 1024;

// Checked at compile time rather than in a test. A test can be filtered out
// or ignored; this cannot, and it fails at the moment someone edits the
// constant rather than the next time the suite runs.
const _: () = assert!(
    MAX_ADDRESS_SPACE < 1024 * 1024 * 1024,
    "the address space cap must stay small enough that a peer cannot make a \
     viewer's machine swap"
);
const _: () = assert!(
    MAX_ADDRESS_SPACE > 256 * 1024 * 1024,
    "and large enough for an 8K decoder's working set"
);

/// Files the decoder may have open.
///
/// Three for its own pipes, and a small allowance so a failure is an error
/// rather than a crash. A decoder that needs a fourth file is a decoder doing
/// something it should not.
pub const MAX_OPEN_FILES: u64 = 16;

/// The environment variable that turns enforcement into logging.
pub const AUDIT_ENV: &str = "ESPER_SANDBOX_AUDIT";

/// What actually got applied.
///
/// Every field is reported to the parent and, through it, to the session HUD.
/// A viewer should be able to find out that its decoder is unsandboxed rather
/// than assume it is not.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SandboxReport {
    /// The address space limit is in force.
    pub address_space: bool,
    /// The open file limit is in force.
    pub open_files: bool,
    /// New privileges are refused, which seccomp requires.
    pub no_new_privs: bool,
    /// Landlock is denying filesystem access.
    pub landlock: bool,
    /// A seccomp filter is loaded.
    pub seccomp: bool,
    /// The seccomp filter kills on an unexpected syscall, rather than logging.
    pub enforcing: bool,
}

impl SandboxReport {
    /// Whether this is a sandbox rather than a gesture at one.
    ///
    /// Requires seccomp *and* enforcement. Landlock is not required, because a
    /// kernel without it still gets a filter that blocks `openat` outright —
    /// but its absence is reported, because two layers were the intent.
    #[must_use]
    pub const fn is_enforcing(&self) -> bool {
        self.seccomp && self.enforcing && self.address_space && self.no_new_privs
    }

    /// A line for the log and the session HUD.
    #[must_use]
    pub fn describe(&self) -> String {
        if !self.seccomp {
            return "NOT SANDBOXED: no seccomp filter is loaded".to_owned();
        }
        let mut parts = Vec::new();
        if self.enforcing {
            parts.push("seccomp enforcing");
        } else {
            parts.push("seccomp AUDIT ONLY — not a sandbox");
        }
        if self.landlock {
            parts.push("landlock");
        } else {
            parts.push("no landlock");
        }
        if self.address_space {
            parts.push("address space capped");
        }
        if self.no_new_privs {
            parts.push("no new privileges");
        }
        parts.join(", ")
    }
}

/// What went wrong applying the sandbox.
#[derive(Debug, thiserror::Error)]
pub enum SandboxError {
    /// A limit could not be set.
    ///
    /// Fatal, deliberately. A decoder that could not be confined must not run:
    /// the alternative is a large C parser on a peer's input with nothing
    /// around it, which is the arrangement this whole module exists to
    /// prevent.
    #[error("could not set {what}: {detail}")]
    Limit { what: &'static str, detail: String },

    /// The seccomp filter could not be built or loaded.
    #[error("could not install the syscall filter: {detail}")]
    Seccomp { detail: String },

    /// This platform has no sandbox.
    #[error("no sandbox is available on this platform")]
    Unsupported,
}

/// Applies the jail.
#[derive(Debug, Clone, Copy)]
pub struct Sandbox {
    /// Address space cap, in bytes.
    pub max_address_space: u64,
    /// Open file cap.
    pub max_open_files: u64,
    /// Log instead of killing. See [`AUDIT_ENV`].
    pub audit_only: bool,
}

impl Default for Sandbox {
    fn default() -> Self {
        Self {
            max_address_space: MAX_ADDRESS_SPACE,
            max_open_files: MAX_OPEN_FILES,
            audit_only: std::env::var_os(AUDIT_ENV).is_some(),
        }
    }
}

impl Sandbox {
    /// Apply every layer, in the order they have to go on.
    ///
    /// Returns what actually took effect. A layer that cannot be applied at
    /// all is an error rather than a smaller sandbox, with one exception:
    /// Landlock on a kernel that does not have it, which is reported as absent
    /// because the seccomp filter already blocks the syscalls it would have
    /// covered.
    #[cfg(target_os = "linux")]
    pub fn apply(&self) -> Result<SandboxReport, SandboxError> {
        let mut report = SandboxReport::default();

        // 1. Limits first, because they are the only layer that still works
        //    if everything after it fails, and because seccomp will block
        //    `prlimit64` afterwards.
        self.set_limits(&mut report)?;

        // 2. `no_new_privs` before seccomp: without it, loading a filter
        //    requires CAP_SYS_ADMIN, and a decoder should not have that.
        rustix::thread::set_no_new_privs(true).map_err(|e| SandboxError::Limit {
            what: "no_new_privs",
            detail: e.to_string(),
        })?;
        report.no_new_privs = true;

        // 3. Landlock before seccomp, because applying it needs syscalls the
        //    filter forbids.
        report.landlock = self.apply_landlock();

        // 4. The syscall filter last. Nothing after this line may use a
        //    syscall the filter does not allow.
        self.apply_seccomp()?;
        report.seccomp = true;
        report.enforcing = !self.audit_only;

        Ok(report)
    }

    /// On anything but Linux there is no sandbox, and saying so is the whole
    /// point.
    ///
    /// Returns an error rather than an empty report: a caller that treated
    /// "no sandbox" as a warning would ship a controller with a large C parser
    /// on its network input path and a HUD claiming otherwise. Windows and
    /// macOS get their own implementations — AppContainer and the App Sandbox —
    /// before the decoder runs there.
    #[cfg(not(target_os = "linux"))]
    pub fn apply(&self) -> Result<SandboxReport, SandboxError> {
        Err(SandboxError::Unsupported)
    }

    #[cfg(target_os = "linux")]
    fn set_limits(&self, report: &mut SandboxReport) -> Result<(), SandboxError> {
        use rustix::process::{Resource, Rlimit, setrlimit};

        let cap = |what: &'static str, resource: Resource, limit: u64| {
            setrlimit(
                resource,
                Rlimit {
                    current: Some(limit),
                    maximum: Some(limit),
                },
            )
            .map_err(|e| SandboxError::Limit {
                what,
                detail: e.to_string(),
            })
        };

        cap("address space", Resource::As, self.max_address_space)?;
        report.address_space = true;
        cap("open files", Resource::Nofile, self.max_open_files)?;
        report.open_files = true;

        // No new processes. A decoder has nothing to fork, and forbidding it
        // means a compromise cannot spawn anything even if it finds a way to
        // call `clone` — belt as well as the filter's braces.
        cap("processes", Resource::Nproc, 0)?;

        // **No `RLIMIT_FSIZE`, and this was a bug worth keeping the note for.**
        //
        // A cap of zero looked obviously right: a decoder writes no files.
        // What it actually does is kill the process with `SIGXFSZ` the moment
        // it writes to *any* descriptor referring to a regular file — which
        // includes its own stderr whenever the parent's stderr is a log file,
        // a CI log, or a shell redirect. The first probe run died of signal 25
        // for the crime of printing a diagnostic.
        //
        // It was also unnecessary. Landlock denies every path and the filter
        // has no `openat`, so the child cannot open a file at all; the only
        // descriptors it has are the ones the parent deliberately handed it.
        // Refusing to let it write to those is not a security boundary, it is
        // an outage that appears only for users who redirect their logs.

        // No core dump. A core of the decoder contains the picture it was
        // decoding, which is the user's screen — and it would be written
        // somewhere neither Landlock nor the filter can see, because the
        // kernel writes it on the process's behalf.
        cap("core dump size", Resource::Core, 0)?;
        Ok(())
    }

    /// Deny all filesystem access.
    ///
    /// Landlock works by granting nothing: a ruleset that handles every access
    /// right, with no rules added, permits none of them. Returns whether it
    /// took effect — a kernel without Landlock is reported rather than fatal,
    /// because the seccomp filter already blocks the syscalls that would reach
    /// a file.
    #[cfg(target_os = "linux")]
    fn apply_landlock(&self) -> bool {
        use landlock::{ABI, Access, AccessFs, Ruleset, RulesetAttr, RulesetStatus};

        // The oldest ABI, so this works on any kernel that has Landlock at
        // all. A newer one would cover more access types, and covering more
        // matters less than working everywhere: the filter is the layer that
        // has to be complete.
        let abi = ABI::V1;

        // **No rules added.** A ruleset that handles every access right and
        // grants none permits nothing — which is what a decoder needs, since
        // it has its pipes and no business with a file.
        let result = Ruleset::default()
            .handle_access(AccessFs::from_all(abi))
            .and_then(Ruleset::create)
            .and_then(|created| created.restrict_self());

        match result {
            Ok(status) => match status.ruleset {
                RulesetStatus::FullyEnforced => true,
                // Partially enforced means a newer access type this kernel
                // does not know about. With ABI::V1 that should not happen,
                // and if it does the honest answer is that the intended
                // sandbox is not fully in place.
                RulesetStatus::PartiallyEnforced => {
                    tracing::warn!("landlock is only partially enforced");
                    false
                }
                RulesetStatus::NotEnforced => false,
            },
            Err(e) => {
                tracing::info!(error = %e, "landlock is unavailable on this kernel");
                false
            }
        }
    }

    /// Install the syscall filter.
    #[cfg(target_os = "linux")]
    fn apply_seccomp(&self) -> Result<(), SandboxError> {
        use seccompiler::{BpfProgram, SeccompAction, SeccompFilter, TargetArch};

        let arch = if cfg!(target_arch = "x86_64") {
            TargetArch::x86_64
        } else if cfg!(target_arch = "aarch64") {
            TargetArch::aarch64
        } else {
            return Err(SandboxError::Seccomp {
                detail: "no syscall filter is defined for this architecture".to_owned(),
            });
        };

        let mismatch = if self.audit_only {
            // Log and allow. Not a sandbox; see the module documentation.
            SeccompAction::Log
        } else {
            // Kill the whole process, not the thread. A thread dying inside a
            // decode leaves the rest of the child holding a half-decoded
            // picture and no idea why.
            SeccompAction::KillProcess
        };

        // **No environment variable widens this list.** A diagnostic that
        // took a numeric range from the environment was used to find the
        // missing `openat` — and it was a hole in its own right, because
        // `ESPER_SANDBOX_EXTRA=0-450` disables the filter entirely. A
        // sandbox with an environment-controlled escape is not a sandbox; the
        // diagnostic was deleted rather than left behind a flag.
        let mut allowed = allowed_syscalls();
        allowed.sort_unstable();
        allowed.dedup();
        let rules = allowed.into_iter().map(|nr| (nr, Vec::new())).collect();

        let filter =
            SeccompFilter::new(rules, mismatch, SeccompAction::Allow, arch).map_err(|e| {
                SandboxError::Seccomp {
                    detail: e.to_string(),
                }
            })?;

        let program: BpfProgram =
            filter
                .try_into()
                .map_err(|e: seccompiler::BackendError| SandboxError::Seccomp {
                    detail: e.to_string(),
                })?;
        seccompiler::apply_filter(&program).map_err(|e| SandboxError::Seccomp {
            detail: e.to_string(),
        })
    }
}

/// The syscalls a decoder actually needs, and nothing else.
///
/// # How this list is justified, and how it is not
///
/// Each entry is here because a decoder's *role* requires it: the pipes, the
/// allocator, timing, and the path a panic takes to terminate. That is the
/// justification, and it is deliberately not "a probe stopped working when I
/// removed it".
///
/// Removing entries one at a time against the sandbox probe says which ones
/// *that probe* exercises, and the answer is six: `exit_group`, `mprotect`,
/// `munmap`, `read`, `sigaltstack`, `write`. Trimming to those six would be a
/// mistake, and the reason is worth stating because it is not obvious:
///
/// - `mmap` reads as removable only because the allocator was warmed before
///   the jail. A 1080p picture is 3 MB and would map fresh.
/// - `futex` reads as removable because nothing contended a lock. Something
///   eventually will.
/// - `tgkill`, `getpid`, `gettid`, `rt_sigreturn` read as removable because
///   the probe never panicked. A panic that cannot reach `abort` becomes a
///   hang instead of a crash, which is worse.
/// - `sched_getaffinity` reads as removable because openh264 asked how many
///   CPUs there were during setup. A resolution change may ask again.
///
/// **A probe under-approximates, always.** A filter minimised against one is a
/// filter that kills the decoder on the first path the probe did not take —
/// on a user's machine, months later, with no clue why. So the bisection is
/// used to find what is *missing*, never to decide what to drop.
///
/// Three were dropped, on the grounds that no decoder role needs them:
/// `clock_nanosleep` (it never sleeps), `sched_yield` (speculative), and
/// `exit` (the child is single-threaded, so `exit_group` is the one that runs).
///
/// Deliberately a short, commented list rather than a category. `@system-service`
/// and similar shorthands are convenient and include networking, filesystem
/// traversal and process control — every one of which is a thing this process
/// must not do.
///
/// Notably absent, and each for a reason a reader should be able to check:
///
/// - **No `openat`, `open`, `stat`.** The decoder has its pipes and needs no
///   files. This is the layer that makes Landlock's absence survivable.
/// - **No `socket`, `connect`, `sendto`.** A compromised decoder must not be
///   able to reach the network — which is what turns a parsing bug into an
///   exfiltration channel.
/// - **No `clone`, `fork`, `execve`.** Nothing to spawn.
/// - **No `ptrace`, `process_vm_readv`.** Nothing to inspect.
/// - **No `prlimit64`.** The limits are already set and must not be raised.
#[cfg(target_os = "linux")]
fn allowed_syscalls() -> Vec<i64> {
    use linux_raw_sys::general as nr;
    // `as i64` on each, because the filter wants signed numbers and the
    // constants are unsigned.
    vec![
        // The pipes. All the decoder does is read bytes and write pictures.
        nr::__NR_read as i64,
        nr::__NR_write as i64,
        // Allocation. openh264 allocates per picture, and the allocator
        // returns memory to the kernel between frames.
        nr::__NR_mmap as i64,
        nr::__NR_munmap as i64,
        nr::__NR_mremap as i64,
        nr::__NR_brk as i64,
        nr::__NR_mprotect as i64,
        // glibc's allocator uses madvise to release pages.
        nr::__NR_madvise as i64,
        // Rust's std and the allocator use futexes for locks. Without this a
        // single contended lock kills the process.
        nr::__NR_futex as i64,
        // Reading the clock, for the decoder's own timing. Usually the vDSO
        // handles this without a syscall, but not on every kernel.
        nr::__NR_clock_gettime as i64,
        // Yielding, which some allocators do under contention.
        // Exiting, and returning from a signal handler — needed or a fatal
        // signal cannot be delivered cleanly.
        nr::__NR_exit_group as i64,
        nr::__NR_rt_sigreturn as i64,
        // Rust's panic handler writes to stderr and then aborts.
        nr::__NR_rt_sigprocmask as i64,
        nr::__NR_getpid as i64,
        nr::__NR_gettid as i64,
        nr::__NR_tgkill as i64,
        // openh264 asks how many CPUs there are. Verified needed: removing
        // this kills the decoder.
        nr::__NR_sched_getaffinity as i64,
        // Rust's runtime removes the signal alternate stack it installed for
        // stack-overflow detection when the process exits. Verified needed:
        // without it a decoder that finishes cleanly dies of SIGSYS *after*
        // its work is done, which looks exactly like a decoder that cannot
        // run sandboxed.
        nr::__NR_sigaltstack as i64,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The bounds on the constant itself are compile-time assertions above,
    /// which cannot be skipped. This checks that the default configuration
    /// actually uses them rather than something else.
    #[test]
    fn the_default_configuration_uses_the_documented_limits() {
        let s = Sandbox::default();
        assert_eq!(s.max_address_space, MAX_ADDRESS_SPACE);
        assert_eq!(s.max_open_files, MAX_OPEN_FILES);
    }

    /// A report has to be able to say "not sandboxed", because a viewer that
    /// cannot tell the difference is a viewer being told a comforting story.
    #[test]
    fn a_report_without_seccomp_says_so_plainly() {
        let none = SandboxReport::default();
        assert!(!none.is_enforcing());
        assert!(
            none.describe().contains("NOT SANDBOXED"),
            "{}",
            none.describe()
        );
    }

    /// Audit mode is not a sandbox and must never read as one.
    #[test]
    fn audit_mode_is_not_reported_as_enforcing() {
        let audit = SandboxReport {
            address_space: true,
            open_files: true,
            no_new_privs: true,
            landlock: true,
            seccomp: true,
            enforcing: false,
        };
        assert!(
            !audit.is_enforcing(),
            "a filter that logs instead of killing is not enforcement"
        );
        assert!(
            audit.describe().contains("AUDIT ONLY"),
            "{}",
            audit.describe()
        );
    }

    /// Landlock's absence is survivable, because the filter blocks the
    /// syscalls it would have covered. Everything else is not.
    #[test]
    fn landlock_is_the_only_optional_layer() {
        let base = SandboxReport {
            address_space: true,
            open_files: true,
            no_new_privs: true,
            landlock: true,
            seccomp: true,
            enforcing: true,
        };
        assert!(base.is_enforcing());

        assert!(
            SandboxReport {
                landlock: false,
                ..base
            }
            .is_enforcing(),
            "a kernel without landlock still gets a filter that blocks openat"
        );

        for (name, broken) in [
            (
                "seccomp",
                SandboxReport {
                    seccomp: false,
                    ..base
                },
            ),
            (
                "enforcing",
                SandboxReport {
                    enforcing: false,
                    ..base
                },
            ),
            (
                "address space",
                SandboxReport {
                    address_space: false,
                    ..base
                },
            ),
            (
                "no_new_privs",
                SandboxReport {
                    no_new_privs: false,
                    ..base
                },
            ),
        ] {
            assert!(
                !broken.is_enforcing(),
                "a sandbox missing {name} must not report as enforcing"
            );
        }
    }

    /// The list is what a reader audits, so its shape matters: it must be
    /// short, and it must not contain the things the documentation says it
    /// does not.
    #[cfg(target_os = "linux")]
    #[test]
    fn the_syscall_list_is_short_and_excludes_what_it_claims_to() {
        use linux_raw_sys::general as nr;
        let allowed = allowed_syscalls();

        assert!(
            allowed.len() < 30,
            "the filter allows {} syscalls; a list this long stops being \
             something a reader can check",
            allowed.len()
        );

        // Each of these is named in the documentation as absent. A test
        // rather than a comment, because the list is the security boundary.
        let forbidden: [(&str, i64); 12] = [
            ("openat", nr::__NR_openat as i64),
            ("socket", nr::__NR_socket as i64),
            ("connect", nr::__NR_connect as i64),
            ("sendto", nr::__NR_sendto as i64),
            ("clone", nr::__NR_clone as i64),
            ("execve", nr::__NR_execve as i64),
            ("ptrace", nr::__NR_ptrace as i64),
            ("process_vm_readv", nr::__NR_process_vm_readv as i64),
            ("prlimit64", nr::__NR_prlimit64 as i64),
            ("kill", nr::__NR_kill as i64),
            ("mount", nr::__NR_mount as i64),
            ("bpf", nr::__NR_bpf as i64),
        ];
        for (name, nr) in forbidden {
            assert!(
                !allowed.contains(&nr),
                "{name} is on the allow list, and the documentation says it is not"
            );
        }

        // And the ones it cannot work without.
        for (name, nr) in [
            ("read", nr::__NR_read as i64),
            ("write", nr::__NR_write as i64),
            ("mmap", nr::__NR_mmap as i64),
            ("exit_group", nr::__NR_exit_group as i64),
        ] {
            assert!(allowed.contains(&nr), "{name} must be allowed");
        }

        // No duplicates: a repeated entry means someone added a syscall
        // twice, which usually means two people thought they needed it for
        // different reasons and neither checked.
        let mut sorted = allowed.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), allowed.len(), "the allow list has duplicates");
    }
}
