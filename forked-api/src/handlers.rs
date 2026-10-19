//! Transport-agnostic API handlers: pure functions from typed requests to
//! typed responses over a [`Forked`] engine. Both the axum server and the
//! Cloudflare Durable Object call these.

use forked_core::{CommitInput, EffectOutcome, Forked, Hash, Op, Storage, DEFAULT_BRANCH};

use crate::dto::*;
use crate::error::{ApiError, ApiResult};

fn parse_hash(hex: &str) -> ApiResult<Hash> {
    Hash::from_hex(hex).map_err(|e| ApiError::invalid(e.to_string()))
}

/// Create a session. The caller resolves the session id (generated or
/// client-chosen) before calling.
pub async fn create_session<S: Storage>(
    f: &Forked<S>,
    session_id: String,
) -> ApiResult<CreateSessionResponse> {
    let head = f.create_session(&session_id).await?;
    Ok(CreateSessionResponse {
        session_id,
        branch: DEFAULT_BRANCH.to_string(),
        head: head.to_hex(),
    })
}

/// Read a branch's full state.
pub async fn get_state<S: Storage>(
    f: &Forked<S>,
    session: &str,
    branch: &str,
) -> ApiResult<StateResponse> {
    let head = f.head(session, branch).await?;
    let commit = f.read_commit(&head).await?;
    let state = f.snapshot_at(&commit.root).await?;
    Ok(StateResponse {
        head: head.to_hex(),
        state,
    })
}

/// Commit ops/meta/effects onto a branch.
pub async fn commit<S: Storage>(
    f: &Forked<S>,
    session: &str,
    branch: &str,
    req: CommitRequest,
) -> ApiResult<HeadResponse> {
    let input = CommitInput {
        ops: req
            .ops
            .into_iter()
            .map(|op| match op {
                OpDto::Put { key, value } => Op::Put(key, value),
                OpDto::Delete { key } => Op::Delete(key),
            })
            .collect(),
        meta: req.meta,
        effects: req.effects.into_iter().map(Into::into).collect(),
    };
    let head = match req.expected_head {
        Some(hex) => {
            let expected = parse_hash(&hex)?;
            f.commit_at(session, branch, &expected, input).await?
        }
        None => f.commit(session, branch, input).await?,
    };
    Ok(HeadResponse {
        head: head.to_hex(),
    })
}

/// Fork a branch.
pub async fn fork<S: Storage>(
    f: &Forked<S>,
    session: &str,
    from: &str,
    req: ForkRequest,
) -> ApiResult<BranchResponse> {
    let head = f.fork(session, from, &req.branch).await?;
    Ok(BranchResponse {
        branch: req.branch,
        head: head.to_hex(),
    })
}

/// Rewind a branch to a commit.
pub async fn checkout<S: Storage>(
    f: &Forked<S>,
    session: &str,
    branch: &str,
    req: CheckoutRequest,
) -> ApiResult<BranchResponse> {
    let target = parse_hash(&req.commit)?;
    f.checkout(session, branch, &target).await?;
    Ok(BranchResponse {
        branch: branch.to_string(),
        head: req.commit,
    })
}

/// List branches.
pub async fn branches<S: Storage>(f: &Forked<S>, session: &str) -> ApiResult<BranchesResponse> {
    let branches = f.branches(session).await?;
    if branches.is_empty() {
        return Err(ApiError::not_found(format!("session {session}")));
    }
    Ok(BranchesResponse {
        branches: branches.into_iter().map(branch_dto).collect(),
    })
}

/// Walk a branch's history.
pub async fn log<S: Storage>(
    f: &Forked<S>,
    session: &str,
    branch: &str,
    limit: Option<usize>,
) -> ApiResult<LogResponse> {
    let entries = f.log(session, branch, limit).await?;
    Ok(LogResponse {
        entries: entries.into_iter().map(commit_dto).collect(),
    })
}

/// Diff two commits.
pub async fn diff<S: Storage>(f: &Forked<S>, a: &str, b: &str) -> ApiResult<DiffResponse> {
    let a = parse_hash(a)?;
    let b = parse_hash(b)?;
    let entries = f.diff(&a, &b).await?;
    Ok(DiffResponse {
        entries: entries.into_iter().map(diff_entry_dto).collect(),
    })
}

/// The whole session graph.
pub async fn graph<S: Storage>(f: &Forked<S>, session: &str) -> ApiResult<GraphResponse> {
    let g = f.graph(session).await?;
    Ok(GraphResponse {
        branches: g.branches.into_iter().map(branch_dto).collect(),
        anchors: g.anchors.into_iter().map(|h| h.to_hex()).collect(),
        commits: g.commits.into_iter().map(commit_dto).collect(),
    })
}

/// Look up an effect by idempotency key without recording anything, the
/// probe half of the effectively-once recipe (check, work, record).
pub async fn find_effect<S: Storage>(
    f: &Forked<S>,
    session: &str,
    key: &str,
) -> ApiResult<FindEffectResponse> {
    let record = f.find_effect(session, key).await?;
    Ok(FindEffectResponse {
        record: record.map(Into::into),
    })
}

/// Record an effect idempotently.
pub async fn record_effect<S: Storage>(
    f: &Forked<S>,
    session: &str,
    req: RecordEffectRequest,
) -> ApiResult<RecordEffectResponse> {
    let branch = req.branch.as_deref().unwrap_or(DEFAULT_BRANCH);
    let record = forked_core::EffectRecord {
        key: req.key,
        kind: req.kind,
        payload: req.payload,
        result: req.result,
    };
    match f.record_effect(session, branch, record).await? {
        EffectOutcome::Recorded { commit, record } => Ok(RecordEffectResponse {
            status: "recorded".to_string(),
            record: record.into(),
            commit: Some(commit.to_hex()),
        }),
        EffectOutcome::AlreadyRecorded(record) => Ok(RecordEffectResponse {
            status: "already_recorded".to_string(),
            record: record.into(),
            commit: None,
        }),
    }
}
