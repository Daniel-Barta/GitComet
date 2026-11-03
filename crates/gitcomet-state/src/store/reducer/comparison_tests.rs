use super::*;
use crate::model::{AppState, Loadable, RepoState};
use crate::msg::{CommitSelectMode, Effect};
use crate::store::repository_runtime::RepositoryRuntimes;
use gitcomet_core::domain::{
    Commit, CommitFileChange, CommitId, FileStatusKind, LogPage, RepoSpec,
};
use gitcomet_core::process::{GitExecutableAvailability, GitExecutablePreference, GitRuntimeState};
use std::sync::atomic::AtomicU64;

fn commit(id: &str, parent: &str) -> Commit {
    Commit {
        id: CommitId(id.into()),
        parent_ids: smallvec::smallvec![CommitId(parent.into())],
        summary: id.into(),
        author: "Tester".into(),
        time: std::time::SystemTime::UNIX_EPOCH,
    }
}

/// A repo whose loaded log is newest-first `c3, c2, c1` (so `c1` is oldest).
/// Every commit has a parent — including the oldest, whose parent `c0` is
/// simply older than the loaded page — so the merged-diff base is a real
/// parent rather than the root-commit fallback.
fn state_with_log(repo_id: RepoId) -> AppState {
    let mut state = AppState {
        git_runtime: GitRuntimeState {
            preference: GitExecutablePreference::SystemPath,
            availability: GitExecutableAvailability::Available {
                version_output: "git version 2.0.0".to_string(),
            },
        },
        ..Default::default()
    };
    let mut repo_state = RepoState::new_opening(
        repo_id,
        RepoSpec {
            workdir: std::path::PathBuf::from("/tmp/repo"),
        },
    );
    repo_state.history_state.log = Loadable::Ready(Arc::new(LogPage {
        commits: vec![commit("c3", "c2"), commit("c2", "c1"), commit("c1", "c0")],
        next_cursor: None,
    }));
    state.repos.push(repo_state);
    state.active_repo = Some(repo_id);
    state
}

fn dispatch_effects(state: &mut AppState, msg: Msg) -> Vec<Effect> {
    let mut repos: RepositoryRuntimes = Default::default();
    let id_alloc = AtomicU64::new(99);
    reduce(&mut repos, &id_alloc, state, msg)
}

fn repo(state: &AppState, repo_id: RepoId) -> &RepoState {
    state.repos.iter().find(|r| r.id == repo_id).unwrap()
}

fn select(state: &mut AppState, repo_id: RepoId, id: &str, mode: CommitSelectMode) -> Vec<Effect> {
    dispatch_effects(
        state,
        Msg::SelectCommitMulti {
            request_id: None,
            repo_id,
            commit_id: CommitId(id.into()),
            mode,
            clicked_index: None,
            visible_order: None,
        },
    )
}

#[test]
fn selecting_two_commits_enters_ordered_range_comparison() {
    let repo_id = RepoId(1);
    let mut state = state_with_log(repo_id);
    let c1 = CommitId("c1".into());
    let c3 = CommitId("c3".into());

    select(&mut state, repo_id, "c2", CommitSelectMode::Single);
    let effects = select(&mut state, repo_id, "c3", CommitSelectMode::Toggle);

    let range = repo(&state, repo_id)
        .history_state
        .range_selection
        .clone()
        .expect("two selected commits should start a comparison");
    // The base is the *parent* of the oldest selected commit, regardless of
    // click order, so the merged diff includes that commit's own changes.
    assert_eq!(range.from, c1);
    assert_eq!(range.to, Some(c3.clone()));

    // The diff pane stays empty: the comparison presents the file
    // side-selection first, and the user opens a file to view its diff.
    assert_eq!(repo(&state, repo_id).diff_state.diff_target, None);
    assert!(matches!(
        repo(&state, repo_id).history_state.range_files,
        Loadable::Loading
    ));
    assert!(
        effects.iter().any(|e| matches!(
            e,
            Effect::LoadRangeFiles { from, to, .. } if *from == c1 && *to == Some(c3.clone())
        )),
        "a LoadRangeFiles effect for c1->c3 should be issued"
    );
}

#[test]
fn range_files_loaded_populates_only_the_current_comparison() {
    let repo_id = RepoId(1);
    let mut state = state_with_log(repo_id);
    select(&mut state, repo_id, "c3", CommitSelectMode::Single);
    select(&mut state, repo_id, "c2", CommitSelectMode::Toggle);
    let effects = select(&mut state, repo_id, "c1", CommitSelectMode::Toggle);
    let request = effects
        .iter()
        .find_map(|e| match e {
            Effect::LoadRangeFiles { request, .. } => Some(*request),
            _ => None,
        })
        .expect("a range-file load should be issued");

    let files = vec![
        CommitFileChange::new(std::path::PathBuf::from("a.rs"), FileStatusKind::Modified)
            .with_line_stats((Some(1), Some(0)).into()),
    ];

    // A stale result (wrong `from`) is dropped.
    dispatch_effects(
        &mut state,
        Msg::Internal(crate::msg::InternalMsg::RangeFilesLoaded {
            repo_id,
            from: CommitId("c9".into()),
            to: Some(CommitId("c3".into())),
            request,
            result: Ok(gitcomet_core::services::Comparison::new(
                CommitId("c9".into()),
                files.clone(),
            )),
        }),
    );
    assert!(matches!(
        repo(&state, repo_id).history_state.range_files,
        Loadable::Loading
    ));

    // A reply from an *overtaken* load for the very same endpoints is
    // dropped too. This is the case `(from, to)` cannot catch: a
    // commit↔working-tree comparison keeps its pair across every refresh, so
    // only the request id distinguishes a current reply from a late one.
    dispatch_effects(
        &mut state,
        Msg::Internal(crate::msg::InternalMsg::RangeFilesLoaded {
            repo_id,
            from: CommitId("c0".into()),
            to: Some(CommitId("c3".into())),
            request: request.wrapping_sub(1),
            result: Ok(gitcomet_core::services::Comparison::new(
                CommitId("c0".into()),
                files.clone(),
            )),
        }),
    );
    assert!(matches!(
        repo(&state, repo_id).history_state.range_files,
        Loadable::Loading
    ));

    // The matching result populates the list.
    dispatch_effects(
        &mut state,
        Msg::Internal(crate::msg::InternalMsg::RangeFilesLoaded {
            repo_id,
            from: CommitId("c0".into()),
            to: Some(CommitId("c3".into())),
            request,
            result: Ok(gitcomet_core::services::Comparison::new(
                CommitId("c0".into()),
                files.clone(),
            )),
        }),
    );
    match &repo(&state, repo_id).history_state.range_files {
        Loadable::Ready(loaded) => assert_eq!(loaded.as_ref(), &files),
        other => panic!("expected loaded range files, got {other:?}"),
    }
}

#[test]
fn single_selection_clears_an_active_comparison() {
    let repo_id = RepoId(1);
    let mut state = state_with_log(repo_id);
    select(&mut state, repo_id, "c3", CommitSelectMode::Single);
    select(&mut state, repo_id, "c2", CommitSelectMode::Toggle);
    assert!(
        repo(&state, repo_id)
            .history_state
            .range_selection
            .is_some()
    );

    select(&mut state, repo_id, "c2", CommitSelectMode::Single);
    assert!(
        repo(&state, repo_id)
            .history_state
            .range_selection
            .is_none(),
        "collapsing to a single commit ends the comparison"
    );
}

fn loaded_details(id: &str) -> gitcomet_core::domain::CommitDetails {
    gitcomet_core::domain::CommitDetails {
        id: CommitId(id.into()),
        message: format!("{id} message"),
        author_name: "Tester".into(),
        author_email: "t@example.com".into(),
        authored_at_unix: 0,
        committed_at: String::new(),
        committed_at_unix: 0,
        parent_ids: Vec::new(),
        files: Vec::new(),
    }
}

/// Entering a comparison moves `selected_commit` to the focused commit
/// without loading its details — the comparison view owns the pane, so that
/// load would be wasted. Leaving the comparison is therefore the moment the
/// details pane has to be put back in sync, or it keeps rendering whichever
/// commit's details were loaded last under a different commit's selection.
#[test]
fn closing_a_comparison_reloads_the_focused_commits_details() {
    let repo_id = RepoId(1);
    let mut state = state_with_log(repo_id);

    // c3 selected, its details loaded.
    select(&mut state, repo_id, "c3", CommitSelectMode::Single);
    state.repos[0].history_state.commit_details = Loadable::Ready(Arc::new(loaded_details("c3")));

    // Ctrl-click c2: comparison mode, focus moves to c2, details stay c3's.
    select(&mut state, repo_id, "c2", CommitSelectMode::Toggle);
    assert!(
        repo(&state, repo_id)
            .history_state
            .range_selection
            .is_some()
    );
    assert_eq!(
        repo(&state, repo_id).history_state.selected_commit,
        Some(CommitId("c2".into()))
    );

    let effects = dispatch_effects(&mut state, Msg::ClearComparison { repo_id });

    let r = repo(&state, repo_id);
    assert!(
        !matches!(&r.history_state.commit_details, Loadable::Ready(d) if d.id == CommitId("c3".into())),
        "c3's details must not stay on screen under c2's selection"
    );
    assert!(
        effects.iter().any(|e| matches!(
            e,
            Effect::LoadCommitDetails { commit_id, .. } if *commit_id == CommitId("c2".into())
        )),
        "closing the comparison should load the still-selected commit's details"
    );
}

/// Every plain history click leaves a commit in `multi_selection`, so a
/// comparison started from a context menu finds a stale one sitting there.
/// It describes a different comparison, so it must not survive to name this
/// one or supply its preview cards.
#[test]
fn an_explicit_comparison_drops_a_stale_multi_selection() {
    let repo_id = RepoId(1);
    let mut state = state_with_log(repo_id);
    select(&mut state, repo_id, "c3", CommitSelectMode::Single);
    select(&mut state, repo_id, "c2", CommitSelectMode::Toggle);
    assert!(
        repo(&state, repo_id)
            .history_state
            .multi_selection
            .is_multi(),
        "precondition: a multi-selection comparison is active"
    );

    dispatch_effects(
        &mut state,
        Msg::CompareWithWorkingTree {
            repo_id,
            from: CommitId("c2".into()),
            from_label: "main".into(),
        },
    );

    let r = repo(&state, repo_id);
    assert!(
        r.history_state.multi_selection.commits.is_empty(),
        "the previous selection is not part of this comparison"
    );
    let range = r
        .history_state
        .range_selection
        .clone()
        .expect("the explicit comparison replaces the previous one");
    assert_eq!(range.from, CommitId("c2".into()));
    assert_eq!(range.to, None);
}

/// A multi-selection comparison keeps its selection: there, the selection
/// *is* what is being compared, and the UI names the comparison after it.
#[test]
fn a_multi_selection_comparison_keeps_its_selection() {
    let repo_id = RepoId(1);
    let mut state = state_with_log(repo_id);
    select(&mut state, repo_id, "c3", CommitSelectMode::Single);
    select(&mut state, repo_id, "c2", CommitSelectMode::Toggle);

    let r = repo(&state, repo_id);
    assert!(r.history_state.range_selection.is_some());
    assert!(r.history_state.multi_selection.is_multi());
}

#[test]
fn clear_comparison_dismisses_selection_and_diff() {
    let repo_id = RepoId(1);
    let mut state = state_with_log(repo_id);
    select(&mut state, repo_id, "c3", CommitSelectMode::Single);
    select(&mut state, repo_id, "c2", CommitSelectMode::Toggle);
    assert!(
        repo(&state, repo_id)
            .history_state
            .range_selection
            .is_some()
    );

    dispatch_effects(&mut state, Msg::ClearComparison { repo_id });
    let r = repo(&state, repo_id);
    assert!(r.history_state.range_selection.is_none());
    assert!(!r.history_state.multi_selection.is_multi());
    assert_eq!(r.diff_state.diff_target, None);
}

#[test]
fn mark_then_compare_with_marked_builds_the_range() {
    let repo_id = RepoId(1);
    let mut state = state_with_log(repo_id);
    let c1 = CommitId("c1".into());
    let c3 = CommitId("c3".into());

    // Nothing marked yet: comparing is a no-op.
    let effects = dispatch_effects(
        &mut state,
        Msg::CompareWithMarked {
            repo_id,
            commit_id: c3.clone(),
            label: "c3".into(),
        },
    );
    assert!(effects.is_empty());
    assert!(
        repo(&state, repo_id)
            .history_state
            .range_selection
            .is_none()
    );

    // Mark c1 (base), then compare c3 against it.
    dispatch_effects(
        &mut state,
        Msg::MarkForComparison {
            repo_id,
            commit_id: c1.clone(),
            label: "main".into(),
        },
    );
    dispatch_effects(
        &mut state,
        Msg::CompareWithMarked {
            repo_id,
            commit_id: c3.clone(),
            label: "feature".into(),
        },
    );
    let range = repo(&state, repo_id)
        .history_state
        .range_selection
        .clone()
        .expect("compare with marked should start a comparison");
    assert_eq!(range.from, c1);
    assert_eq!(range.to, Some(c3.clone()));
    assert_eq!(range.from_label, "main");
    assert_eq!(range.to_label, "feature");
}

#[test]
fn compare_commit_range_message_orders_via_labels() {
    let repo_id = RepoId(1);
    let mut state = state_with_log(repo_id);
    let effects = dispatch_effects(
        &mut state,
        Msg::CompareCommitRange {
            repo_id,
            from: CommitId("c1".into()),
            to: CommitId("c3".into()),
            from_label: "main".into(),
            to_label: "feature".into(),
        },
    );
    let range = repo(&state, repo_id)
        .history_state
        .range_selection
        .clone()
        .expect("explicit compare should set a comparison");
    assert_eq!(range.from_label, "main");
    assert_eq!(range.to_label, "feature");
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, Effect::LoadRangeFiles { .. }))
    );
}

#[test]
fn compare_with_working_tree_starts_a_worktree_comparison() {
    let repo_id = RepoId(1);
    let mut state = state_with_log(repo_id);
    let from = CommitId("c2".into());

    let effects = dispatch_effects(
        &mut state,
        Msg::CompareWithWorkingTree {
            repo_id,
            from: from.clone(),
            from_label: "main".into(),
        },
    );

    let range = repo(&state, repo_id)
        .history_state
        .range_selection
        .clone()
        .expect("compare with working tree should start a comparison");
    assert_eq!(range.from, from);
    // The tip is the working tree, not a commit.
    assert_eq!(range.to, None);
    assert_eq!(range.to_label, "Working tree");
    // A worktree-tip file list load is issued, and the diff pane is cleared.
    assert!(effects.iter().any(|e| matches!(
        e,
        Effect::LoadRangeFiles { from: f, to: None, .. } if *f == from
    )));
    assert_eq!(repo(&state, repo_id).diff_state.diff_target, None);
}

/// A refresh means two full-tree `git diff` calls, so changes arriving while
/// one is running must fold into it rather than each starting their own —
/// and the fold must still end with a run that sees the final state.
#[test]
fn external_worktree_changes_refresh_a_worktree_comparison_one_at_a_time() {
    let repo_id = RepoId(1);
    let mut state = state_with_log(repo_id);
    let from = CommitId("c2".into());
    let effects = dispatch_effects(
        &mut state,
        Msg::CompareWithWorkingTree {
            repo_id,
            from: from.clone(),
            from_label: "main".into(),
        },
    );
    let load_request = |effects: &[Effect]| {
        effects.iter().find_map(|e| match e {
            Effect::LoadRangeFiles {
                from: f,
                to: None,
                request,
                ..
            } if *f == from => Some(*request),
            _ => None,
        })
    };
    let first = load_request(&effects).expect("the comparison issues a file-list load");

    // Two changes land while that load is still running: neither starts its
    // own, they collapse into the one already in flight.
    for _ in 0..2 {
        let effects = dispatch_effects(
            &mut state,
            Msg::RepoExternallyChanged {
                repo_id,
                change: crate::msg::RepoExternalChange::Worktree,
            },
        );
        assert!(
            load_request(&effects).is_none(),
            "a refresh must not stack on top of one already in flight"
        );
    }

    // When it lands, the folded changes are honoured by exactly one re-run,
    // so the list ends up describing the worktree as it is now.
    let effects = dispatch_effects(
        &mut state,
        Msg::Internal(crate::msg::InternalMsg::RangeFilesLoaded {
            repo_id,
            from: from.clone(),
            to: None,
            request: first,
            result: Ok(gitcomet_core::services::Comparison::new(
                from.clone(),
                Vec::new(),
            )),
        }),
    );
    let second = load_request(&effects).expect("the folded refresh runs once the load lands");
    assert_ne!(first, second, "the re-run is a new request, not a replay");

    // Nothing further is queued, so a quiet worktree stops the chain.
    let effects = dispatch_effects(
        &mut state,
        Msg::Internal(crate::msg::InternalMsg::RangeFilesLoaded {
            repo_id,
            from: from.clone(),
            to: None,
            request: second,
            result: Ok(gitcomet_core::services::Comparison::new(
                from.clone(),
                Vec::new(),
            )),
        }),
    );
    assert!(load_request(&effects).is_none());

    // And with nothing in flight, the next change refreshes immediately.
    let effects = dispatch_effects(
        &mut state,
        Msg::RepoExternallyChanged {
            repo_id,
            change: crate::msg::RepoExternalChange::Worktree,
        },
    );
    assert!(
        load_request(&effects).is_some(),
        "expected the worktree comparison file list to refresh"
    );
}

#[test]
fn external_change_does_not_refresh_a_commit_comparison() {
    let repo_id = RepoId(1);
    let mut state = state_with_log(repo_id);
    // Two-commit (immutable) comparison.
    select(&mut state, repo_id, "c3", CommitSelectMode::Single);
    select(&mut state, repo_id, "c2", CommitSelectMode::Toggle);
    assert!(
        repo(&state, repo_id)
            .history_state
            .range_selection
            .is_some()
    );

    let effects = dispatch_effects(
        &mut state,
        Msg::RepoExternallyChanged {
            repo_id,
            change: crate::msg::RepoExternalChange::Worktree,
        },
    );
    assert!(
        !effects
            .iter()
            .any(|e| matches!(e, Effect::LoadRangeFiles { .. })),
        "a commit↔commit comparison is immutable and must not refresh"
    );
}

/// `c3` and `c1` with `c2` between them: no diff holds exactly their changes,
/// as `c0..c3` would add `c2`'s. The selection stays a plain list until it
/// closes the gap.
#[test]
fn selecting_commits_with_a_gap_between_them_shows_no_merged_diff() {
    let repo_id = RepoId(1);
    let mut state = state_with_log(repo_id);

    select(&mut state, repo_id, "c3", CommitSelectMode::Single);
    let effects = select(&mut state, repo_id, "c1", CommitSelectMode::Toggle);

    let history = &repo(&state, repo_id).history_state;
    assert!(history.multi_selection.is_multi());
    assert_eq!(history.range_selection, None);
    assert!(
        !effects
            .iter()
            .any(|e| matches!(e, Effect::LoadRangeFiles { .. })),
        "a selection with a gap must not load a merged diff"
    );

    select(&mut state, repo_id, "c2", CommitSelectMode::Toggle);
    let range = repo(&state, repo_id)
        .history_state
        .range_selection
        .clone()
        .expect("closing the gap starts the comparison");
    assert_eq!(range.from, CommitId("c0".into()));
    assert_eq!(range.to, Some(CommitId("c3".into())));
}

/// Two branch tips listed next to each other are not consecutive commits: the
/// diff from one's parent to the other is the difference between branches.
#[test]
fn selecting_commits_on_different_branches_shows_no_merged_diff() {
    let repo_id = RepoId(1);
    let mut state = state_with_log(repo_id);
    state.repos[0].history_state.log = Loadable::Ready(Arc::new(LogPage {
        commits: vec![
            commit("a1", "base"),
            commit("b1", "base"),
            commit("base", "c0"),
        ],
        next_cursor: None,
    }));

    select(&mut state, repo_id, "a1", CommitSelectMode::Single);
    let effects = select(&mut state, repo_id, "b1", CommitSelectMode::Toggle);

    let history = &repo(&state, repo_id).history_state;
    assert!(history.multi_selection.is_multi());
    assert_eq!(history.range_selection, None);
    assert!(
        !effects
            .iter()
            .any(|e| matches!(e, Effect::LoadRangeFiles { .. }))
    );
}

fn range_of(state: &AppState, repo_id: RepoId) -> Option<(CommitId, Option<CommitId>)> {
    repo(state, repo_id)
        .history_state
        .range_selection
        .as_ref()
        .map(|range| (range.from.clone(), range.to.clone()))
}

/// The tip of a branch and a commit merged into it from another branch: not a
/// first-parent chain, so Ctrl-click shows no merged diff, but Ctrl+Shift-click
/// compares the two directly — older to newer, whichever is clicked first.
#[test]
fn ctrl_shift_click_compares_a_commit_merged_in_from_another_branch_directly() {
    let repo_id = RepoId(1);
    for (first, second) in [("tip", "side"), ("side", "tip")] {
        let mut state = state_with_log(repo_id);
        let mut merge = commit("merge", "main");
        merge.parent_ids.push(CommitId("side".into()));
        state.repos[0].history_state.log = Loadable::Ready(Arc::new(LogPage {
            commits: vec![
                commit("tip", "merge"),
                merge,
                commit("main", "base"),
                commit("side", "base"),
                commit("base", "c0"),
            ],
            next_cursor: None,
        }));

        select(&mut state, repo_id, first, CommitSelectMode::Single);
        let effects = select(&mut state, repo_id, second, CommitSelectMode::Compare);

        let side = CommitId("side".into());
        let tip = CommitId("tip".into());
        assert_eq!(
            range_of(&state, repo_id),
            Some((side.clone(), Some(tip.clone()))),
            "{first} then {second}"
        );
        let selection = &repo(&state, repo_id).history_state.multi_selection;
        assert!(selection.compare_pair);
        assert_eq!(
            *selection.commits,
            vec![CommitId(first.into()), CommitId(second.into())]
        );
        assert!(effects.iter().any(|e| matches!(
            e,
            Effect::LoadRangeFiles { from, to, .. } if *from == side && *to == Some(tip.clone())
        )));
    }
}

/// A pair diffs from the older commit itself, not its parent: `c1..c3` holds
/// `c2` and `c3`'s changes, where the merged diff of consecutive commits would
/// start from the oldest one's parent.
#[test]
fn ctrl_shift_click_pair_diffs_from_the_older_commit_not_its_parent() {
    let repo_id = RepoId(1);
    let mut state = state_with_log(repo_id);

    select(&mut state, repo_id, "c3", CommitSelectMode::Single);
    select(&mut state, repo_id, "c1", CommitSelectMode::Compare);
    assert_eq!(
        range_of(&state, repo_id),
        Some((CommitId("c1".into()), Some(CommitId("c3".into()))))
    );

    // The anchor stays on c3, so the next pair replaces c1 rather than
    // growing the selection, and consecutive commits are compared directly too.
    select(&mut state, repo_id, "c2", CommitSelectMode::Compare);
    let history = &repo(&state, repo_id).history_state;
    assert_eq!(
        *history.multi_selection.commits,
        vec![CommitId("c3".into()), CommitId("c2".into())]
    );
    assert_eq!(
        range_of(&state, repo_id),
        Some((CommitId("c2".into()), Some(CommitId("c3".into()))))
    );
}

#[test]
fn ctrl_shift_click_without_another_commit_acts_as_a_plain_click() {
    let repo_id = RepoId(1);
    let mut state = state_with_log(repo_id);

    // Nothing selected yet: there is no anchor to pair with.
    select(&mut state, repo_id, "c2", CommitSelectMode::Compare);
    let history = &repo(&state, repo_id).history_state;
    assert_eq!(
        *history.multi_selection.commits,
        vec![CommitId("c2".into())]
    );
    assert!(!history.multi_selection.compare_pair);
    assert_eq!(history.range_selection, None);

    // The anchor itself: a commit is not compared with itself.
    select(&mut state, repo_id, "c3", CommitSelectMode::Single);
    select(&mut state, repo_id, "c1", CommitSelectMode::Compare);
    select(&mut state, repo_id, "c3", CommitSelectMode::Compare);
    let history = &repo(&state, repo_id).history_state;
    assert_eq!(
        *history.multi_selection.commits,
        vec![CommitId("c3".into())]
    );
    assert!(!history.multi_selection.compare_pair);
    assert_eq!(history.range_selection, None);
}

#[test]
fn ctrl_click_after_a_pair_returns_to_the_merged_diff() {
    let repo_id = RepoId(1);
    let mut state = state_with_log(repo_id);

    select(&mut state, repo_id, "c3", CommitSelectMode::Single);
    select(&mut state, repo_id, "c1", CommitSelectMode::Compare);
    select(&mut state, repo_id, "c2", CommitSelectMode::Toggle);

    let history = &repo(&state, repo_id).history_state;
    assert_eq!(history.multi_selection.commits.len(), 3);
    assert!(!history.multi_selection.compare_pair);
    assert_eq!(
        range_of(&state, repo_id),
        Some((CommitId("c0".into()), Some(CommitId("c3".into()))))
    );
}

/// Right-click inside the pair moves focus for the menu but keeps the pair and
/// its direct comparison.
#[test]
fn right_click_inside_a_pair_keeps_its_direct_comparison() {
    let repo_id = RepoId(1);
    let mut state = state_with_log(repo_id);

    select(&mut state, repo_id, "c3", CommitSelectMode::Single);
    select(&mut state, repo_id, "c1", CommitSelectMode::Compare);
    select(
        &mut state,
        repo_id,
        "c3",
        CommitSelectMode::PreserveIfSelected,
    );

    let history = &repo(&state, repo_id).history_state;
    assert!(history.multi_selection.compare_pair);
    assert_eq!(history.selected_commit, Some(CommitId("c3".into())));
    assert_eq!(
        range_of(&state, repo_id),
        Some((CommitId("c1".into()), Some(CommitId("c3".into()))))
    );
}

/// `state_with_log`, with an uncommitted change, so History shows its
/// "Uncommitted changes" row.
fn state_with_uncommitted_changes(repo_id: RepoId) -> AppState {
    let mut state = state_with_log(repo_id);
    state.repos[0].worktree_status =
        Loadable::Ready(Arc::new(vec![gitcomet_core::domain::FileStatus {
            path: "a.txt".into(),
            kind: FileStatusKind::Modified,
            conflict: None,
        }]));
    state
}

fn compare_anchor_with_working_tree(state: &mut AppState, repo_id: RepoId) -> Vec<Effect> {
    dispatch_effects(
        state,
        Msg::CompareAnchorWithWorkingTree {
            request_id: None,
            repo_id,
        },
    )
}

/// What "Compare with working tree" from the commit menu shows for `from`, so
/// a Ctrl/Cmd+Shift pair can be checked against it.
fn menu_working_tree_comparison(from: &str) -> crate::model::RangeSelection {
    let repo_id = RepoId(1);
    let mut state = state_with_uncommitted_changes(repo_id);
    dispatch_effects(
        &mut state,
        Msg::CompareWithWorkingTree {
            repo_id,
            from: CommitId(from.into()),
            from_label: from.into(),
        },
    );
    repo(&state, repo_id)
        .history_state
        .range_selection
        .clone()
        .unwrap()
}

/// Ctrl/Cmd+Shift-click on the "Uncommitted changes" row pairs the anchor
/// commit with the working tree: the same comparison as the commit menu's
/// "Compare with working tree", with the commit and the row both selected.
#[test]
fn ctrl_shift_click_on_uncommitted_changes_compares_the_anchor_with_the_working_tree() {
    let repo_id = RepoId(1);
    let mut state = state_with_uncommitted_changes(repo_id);
    let c2 = CommitId("c2".into());

    select(&mut state, repo_id, "c2", CommitSelectMode::Single);
    let effects = compare_anchor_with_working_tree(&mut state, repo_id);

    let history = &repo(&state, repo_id).history_state;
    let range = history.range_selection.clone().expect("a comparison");
    let menu = menu_working_tree_comparison("c2");
    assert_eq!(range.from, c2);
    assert_eq!(range.to, None);
    assert_eq!(range.to_label, menu.to_label);
    assert_eq!(range.options, menu.options, "the menu's file list");
    assert!(history.multi_selection.working_tree);
    assert_eq!(*history.multi_selection.commits, vec![c2.clone()]);
    assert_eq!(history.selected_commit, Some(c2.clone()));
    assert!(effects.iter().any(|e| matches!(
        e,
        Effect::LoadRangeFiles { from, to: None, options, .. }
            if *from == c2 && *options == menu.options
    )));

    // The anchor stays on c2: Ctrl/Cmd+Shift on another commit pairs the two
    // commits again.
    select(&mut state, repo_id, "c3", CommitSelectMode::Compare);
    let history = &repo(&state, repo_id).history_state;
    assert!(!history.multi_selection.working_tree);
    assert_eq!(
        range_of(&state, repo_id),
        Some((c2.clone(), Some(CommitId("c3".into()))))
    );
}

/// With the "Uncommitted changes" row selected — as it is when nothing else is
/// — Ctrl/Cmd+Shift on a commit pairs it with the working tree, and the row
/// stays the anchor for further pairs.
#[test]
fn ctrl_shift_click_on_a_commit_pairs_it_with_selected_uncommitted_changes() {
    let repo_id = RepoId(1);
    let mut state = state_with_uncommitted_changes(repo_id);

    for id in ["c1", "c3"] {
        select(&mut state, repo_id, id, CommitSelectMode::Compare);
        let history = &repo(&state, repo_id).history_state;
        assert_eq!(range_of(&state, repo_id), Some((CommitId(id.into()), None)));
        assert!(history.multi_selection.working_tree);
        assert_eq!(history.multi_selection.anchor, None);
        assert_eq!(*history.multi_selection.commits, vec![CommitId(id.into())]);
        assert_eq!(history.selected_commit, Some(CommitId(id.into())));
        assert_eq!(
            history.range_selection.as_ref().unwrap().options,
            menu_working_tree_comparison(id).options
        );
    }

    // A plain click leaves the pair.
    select(&mut state, repo_id, "c2", CommitSelectMode::Single);
    let history = &repo(&state, repo_id).history_state;
    assert!(!history.multi_selection.working_tree);
    assert_eq!(history.range_selection, None);
}

/// Without uncommitted changes there is no row to pair with: nothing selected
/// falls back to HEAD, and Ctrl/Cmd+Shift acts as a plain click, as before.
#[test]
fn ctrl_shift_click_without_uncommitted_changes_acts_as_a_plain_click() {
    let repo_id = RepoId(1);
    let mut state = state_with_log(repo_id);

    select(&mut state, repo_id, "c2", CommitSelectMode::Compare);
    let history = &repo(&state, repo_id).history_state;
    assert!(!history.multi_selection.working_tree);
    assert_eq!(history.range_selection, None);
}

/// Nothing to pair with the working tree: the click selects the row.
#[test]
fn ctrl_shift_click_on_uncommitted_changes_without_a_commit_selects_the_row() {
    let repo_id = RepoId(1);
    let mut state = state_with_uncommitted_changes(repo_id);

    compare_anchor_with_working_tree(&mut state, repo_id);
    let history = &repo(&state, repo_id).history_state;
    assert_eq!(history.selected_commit, None);
    assert_eq!(history.range_selection, None);
    assert!(repo(&state, repo_id).uncommitted_changes_row_selected());
}

/// Ctrl-click leaves a working-tree pair for the ordinary multi-selection.
#[test]
fn ctrl_click_after_a_working_tree_pair_leaves_it() {
    let repo_id = RepoId(1);
    let mut state = state_with_uncommitted_changes(repo_id);

    select(&mut state, repo_id, "c3", CommitSelectMode::Compare);
    select(&mut state, repo_id, "c2", CommitSelectMode::Toggle);
    let history = &repo(&state, repo_id).history_state;
    assert!(!history.multi_selection.working_tree);
    assert_eq!(
        *history.multi_selection.commits,
        vec![CommitId("c3".into()), CommitId("c2".into())]
    );
    assert_eq!(
        range_of(&state, repo_id),
        Some((CommitId("c1".into()), Some(CommitId("c3".into()))))
    );
}

/// The list landing again can change the selected file's change: deleted on
/// disk while a working-tree comparison shows it, say. The diff then loads
/// the one side it has left instead of a patch against nothing.
#[test]
fn comparison_list_refresh_replans_a_file_that_lost_a_side() {
    let repo_id = RepoId(1);
    let mut state = state_with_log(repo_id);
    let c1 = CommitId("c1".into());
    let c3 = CommitId("c3".into());
    select(&mut state, repo_id, "c2", CommitSelectMode::Single);
    select(&mut state, repo_id, "c3", CommitSelectMode::Toggle);
    let land = |state: &mut AppState, kind: FileStatusKind| {
        let request = state.repos[0].begin_range_files_load();
        dispatch_effects(
            state,
            Msg::Internal(crate::msg::InternalMsg::RangeFilesLoaded {
                repo_id,
                from: CommitId("c1".into()),
                to: Some(CommitId("c3".into())),
                request,
                result: Ok(gitcomet_core::services::Comparison::new(
                    CommitId("c1".into()),
                    vec![CommitFileChange::new(
                        std::path::PathBuf::from("a.rs"),
                        kind,
                    )],
                )),
            }),
        )
    };
    land(&mut state, FileStatusKind::Modified);
    dispatch_effects(
        &mut state,
        Msg::SelectDiff {
            repo_id,
            target: gitcomet_core::domain::DiffTarget::commit_range(
                c1,
                Some(c3),
                Some(std::path::PathBuf::from("a.rs")),
            ),
        },
    );
    assert_eq!(repo(&state, repo_id).diff_state.one_sided_side, None);

    let effects = land(&mut state, FileStatusKind::Deleted);
    let diff_state = &repo(&state, repo_id).diff_state;
    assert_eq!(
        diff_state.one_sided_side,
        Some(gitcomet_core::domain::DiffPreviewTextSide::Old)
    );
    assert!(matches!(
        diff_state.diff_preview_text_file,
        Loadable::Loading
    ));
    assert!(matches!(diff_state.diff, Loadable::NotLoaded));
    assert!(
        effects
            .iter()
            .any(|effect| matches!(effect, Effect::LoadDiffPreviewTextFile { .. })),
        "the old side should load: {effects:?}"
    );

    // The same list again changes nothing, so nothing reloads.
    let effects = land(&mut state, FileStatusKind::Deleted);
    assert!(
        !effects
            .iter()
            .any(|effect| matches!(effect, Effect::LoadDiffPreviewTextFile { .. })),
        "{effects:?}"
    );
}
