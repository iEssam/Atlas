//! Application grouping heuristics (PRD §9.2.1, docs/phases.md M3).
//!
//! Groups a snapshot's processes into *application groups*: the family of
//! processes that make up one running application (a browser and its renderer
//! children, an app and its helper workers, ...). This is a **best-effort
//! heuristic**, not a ground truth — without command lines (which the ETW
//! provider doesn't give us) we can only reason from the image base name and the
//! parent chain. It is deliberately conservative: when in doubt a process stands
//! alone rather than being mis-grouped.
//!
//! # Heuristic
//!
//! For each process we derive an *image family*: the lowercased image base name
//! with its `.exe` extension stripped (e.g. `chrome.exe` → `chrome`). Then:
//!
//! * A process whose **parent shares the same image family** is a **helper**:
//!   it joins its parent's group (a browser spawning renderers from the same
//!   binary, a worker pool re-exec-ing the main image). The group key is the
//!   family plus the group root's pid, so two independent instances of the same
//!   app (two separate `chrome` trees) form two groups, not one.
//! * The **group root** — the shallowest process of a family whose parent is
//!   *outside* the family — is the **main**. Its group key names it.
//! * A process in **session 0** (services / service-hosted) is a **service**:
//!   session 0 is the non-interactive services session, so these are grouped as
//!   services regardless of family. `svchost` instances therefore never fold
//!   into an interactive app's group.
//!
//! A standalone interactive process (parent outside its family, no same-family
//! children) is its own group with role Main and a group of size 1.
//!
//! No command-line parsing, no service-name resolution: those need data we
//! don't collect yet (revisit with rundown / SCM in a later milestone).

use std::collections::HashMap;

/// The role a process plays within its application group (mirrors the proto
/// `ProcessRole`, but lives here so the collector has no proto dependency).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessRole {
    /// Root of its application group (its parent is outside the group family).
    Main,
    /// Same-group descendant of the main (helper / renderer / worker).
    Helper,
    /// Session-0 service / service-hosted process.
    Service,
}

/// The minimal per-process facts the grouping heuristic needs. A thin view over
/// a snapshot/sample row so the function is trivially unit-testable without a
/// live snapshot.
#[derive(Debug, Clone)]
pub struct GroupInput {
    pub pid: u32,
    pub parent_pid: u32,
    pub image_name: String,
    pub session_id: u32,
}

/// The grouping verdict for one process: the group key (empty only if the input
/// list was empty for this pid, which cannot happen here) and the role.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupOutput {
    pub app_group: String,
    pub role: ProcessRole,
}

/// The image *family* of an image name: the lowercased base file name with a
/// trailing `.exe` removed. Path separators (both `\\` and `/`) and device-path
/// prefixes are handled by taking the final component. An empty or unknown name
/// yields an empty family (which never matches another family, so such
/// processes never fold together).
pub fn image_family(image_name: &str) -> String {
    let base = image_name
        .rsplit(['\\', '/'])
        .next()
        .unwrap_or(image_name)
        .trim();
    let lowered = base.to_ascii_lowercase();
    lowered
        .strip_suffix(".exe")
        .map(|s| s.to_string())
        .unwrap_or(lowered)
}

/// Groups a snapshot's processes into application groups, returning a map from
/// pid to its [`GroupOutput`]. Pure and deterministic: the same input always
/// yields the same grouping, independent of iteration order.
///
/// See the module docs for the heuristic. In short: session-0 processes are
/// services; otherwise a process whose parent shares its image family is a
/// helper folded into the group rooted at the family's topmost ancestor (the
/// main), and every other process is a standalone main.
pub fn group_processes(procs: &[GroupInput]) -> HashMap<u32, GroupOutput> {
    // pid → row, for parent-chain walking.
    let by_pid: HashMap<u32, &GroupInput> = procs.iter().map(|p| (p.pid, p)).collect();
    let family: HashMap<u32, String> = procs
        .iter()
        .map(|p| (p.pid, image_family(&p.image_name)))
        .collect();

    // The group root of a process: walk up the parent chain while the parent is
    // present in this snapshot AND shares the same (non-empty) image family. The
    // shallowest same-family ancestor is the group root (the "main"). Guards
    // against cycles and self-parent by bounding the walk and tracking visited.
    let group_root = |start: u32| -> u32 {
        let start_family = match family.get(&start) {
            Some(f) if !f.is_empty() => f.clone(),
            _ => return start, // unknown family: stands alone
        };
        let mut cur = start;
        let mut guard = 0;
        while let Some(row) = by_pid.get(&cur) {
            let parent = row.parent_pid;
            if parent == cur || parent == 0 {
                break; // self-parent or no parent
            }
            // Parent must be in the snapshot and share the family to keep climbing.
            match by_pid.get(&parent) {
                Some(prow) if image_family(&prow.image_name) == start_family => {
                    cur = parent;
                }
                _ => break,
            }
            guard += 1;
            if guard > 4096 {
                break; // pathological chain / cycle safety
            }
        }
        cur
    };

    let mut out = HashMap::with_capacity(procs.len());
    for p in procs {
        // Session 0 is the services session: classify as a service, grouped by
        // its own family so service-hosted processes never merge into an
        // interactive app's group.
        if p.session_id == 0 {
            let fam = family.get(&p.pid).cloned().unwrap_or_default();
            let fam = if fam.is_empty() {
                format!("pid:{}", p.pid)
            } else {
                fam
            };
            out.insert(
                p.pid,
                GroupOutput {
                    app_group: format!("service:{fam}"),
                    role: ProcessRole::Service,
                },
            );
            continue;
        }

        let root = group_root(p.pid);
        let root_family = family.get(&root).cloned().unwrap_or_default();
        let key_family = if root_family.is_empty() {
            format!("pid:{root}")
        } else {
            root_family
        };
        // The group key ties the family to the root pid so two independent trees
        // of the same app are two distinct groups.
        let app_group = format!("app:{key_family}#{root}");
        let role = if root == p.pid {
            ProcessRole::Main
        } else {
            ProcessRole::Helper
        };
        out.insert(p.pid, GroupOutput { app_group, role });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(pid: u32, parent: u32, name: &str, session: u32) -> GroupInput {
        GroupInput {
            pid,
            parent_pid: parent,
            image_name: name.to_string(),
            session_id: session,
        }
    }

    #[test]
    fn image_family_strips_path_and_extension() {
        assert_eq!(image_family("chrome.exe"), "chrome");
        assert_eq!(image_family("Chrome.EXE"), "chrome");
        assert_eq!(
            image_family(r"C:\Program Files\Google\chrome.exe"),
            "chrome"
        );
        assert_eq!(
            image_family(r"\Device\HarddiskVolume4\svchost.exe"),
            "svchost"
        );
        assert_eq!(image_family("weird_no_ext"), "weird_no_ext");
        assert_eq!(image_family(""), "");
    }

    /// Browser-with-renderers: one main chrome, three same-image renderer
    /// children (parented by the main). All share one group; the parent is Main,
    /// children are Helpers.
    #[test]
    fn browser_with_renderers_forms_one_group() {
        let procs = vec![
            p(100, 50, "chrome.exe", 1), // main (parent explorer, outside family)
            p(50, 40, "explorer.exe", 1),
            p(101, 100, "chrome.exe", 1), // renderer
            p(102, 100, "chrome.exe", 1), // renderer
            p(103, 101, "chrome.exe", 1), // nested renderer (grandchild)
        ];
        let g = group_processes(&procs);

        assert_eq!(g[&100].role, ProcessRole::Main);
        let group = &g[&100].app_group;
        // Every chrome process shares the main's group.
        for pid in [101, 102, 103] {
            assert_eq!(g[&pid].role, ProcessRole::Helper);
            assert_eq!(&g[&pid].app_group, group, "renderer joins the main's group");
        }
        // explorer is its own standalone main, a different group.
        assert_eq!(g[&50].role, ProcessRole::Main);
        assert_ne!(&g[&50].app_group, group);
    }

    /// A standalone interactive exe (parent outside its family, no same-family
    /// children) is its own single-member group with role Main.
    #[test]
    fn standalone_exe_is_its_own_main() {
        let procs = vec![p(200, 50, "notepad.exe", 1), p(50, 40, "explorer.exe", 1)];
        let g = group_processes(&procs);
        assert_eq!(g[&200].role, ProcessRole::Main);
        assert_ne!(g[&200].app_group, g[&50].app_group);
    }

    /// svchost (and anything else) in session 0 is a Service, grouped as a
    /// service and never folded into an interactive group even if a same-family
    /// parent exists.
    #[test]
    fn svchost_session0_is_service() {
        let procs = vec![
            p(8, 4, "services.exe", 0),
            p(900, 8, "svchost.exe", 0),
            p(901, 8, "svchost.exe", 0),
        ];
        let g = group_processes(&procs);
        assert_eq!(g[&900].role, ProcessRole::Service);
        assert_eq!(g[&901].role, ProcessRole::Service);
        // Both svchosts share the service:svchost group by family.
        assert_eq!(g[&900].app_group, g[&901].app_group);
        assert!(g[&900].app_group.starts_with("service:"));
        // services.exe is also a service (session 0).
        assert_eq!(g[&8].role, ProcessRole::Service);
    }

    /// Two independent instances of the same app (two separate chrome trees)
    /// form two distinct groups, not one merged group.
    #[test]
    fn two_app_instances_are_distinct_groups() {
        let procs = vec![
            p(100, 50, "app.exe", 1),
            p(101, 100, "app.exe", 1),
            p(200, 60, "app.exe", 1),
            p(201, 200, "app.exe", 1),
            p(50, 40, "explorer.exe", 1),
            p(60, 40, "explorer.exe", 1),
        ];
        let g = group_processes(&procs);
        assert_eq!(g[&100].role, ProcessRole::Main);
        assert_eq!(g[&200].role, ProcessRole::Main);
        assert_ne!(
            g[&100].app_group, g[&200].app_group,
            "separate trees are separate groups"
        );
        assert_eq!(g[&101].app_group, g[&100].app_group);
        assert_eq!(g[&201].app_group, g[&200].app_group);
    }

    /// A same-family child whose parent is NOT in the snapshot (parent already
    /// exited) is treated as its own main — the chain can't be climbed, so it
    /// doesn't get orphaned into a phantom group.
    #[test]
    fn orphaned_same_family_child_becomes_main() {
        let procs = vec![p(300, 299, "worker.exe", 1)]; // parent 299 absent
        let g = group_processes(&procs);
        assert_eq!(g[&300].role, ProcessRole::Main);
    }

    /// A parent cycle (a↔b) does not hang the walk; both resolve deterministically.
    #[test]
    fn parent_cycle_is_bounded() {
        let procs = vec![p(1, 2, "x.exe", 1), p(2, 1, "x.exe", 1)];
        let g = group_processes(&procs);
        // Both are same-family; the walk terminates and assigns roles without
        // hanging. We only assert it produced outputs for both.
        assert!(g.contains_key(&1));
        assert!(g.contains_key(&2));
    }

    #[test]
    fn grouping_is_deterministic() {
        let procs = vec![
            p(100, 50, "chrome.exe", 1),
            p(101, 100, "chrome.exe", 1),
            p(50, 40, "explorer.exe", 1),
        ];
        let a = group_processes(&procs);
        // Reverse the input order; grouping must be identical.
        let mut rev = procs.clone();
        rev.reverse();
        let b = group_processes(&rev);
        assert_eq!(a, b);
    }
}
