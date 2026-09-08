//! The operator gate (spec 024 B-2).
//!
//! An operator surface is the trace ring, ledger inspection, a backup
//! trigger: things that describe the cell rather than serve it. They are
//! gated by the one role the manifest names (`auth.operator_role`) and they
//! carry no rate limiter, because a limiter in front of an operator is a
//! limiter on the person who is trying to find out why the cell is
//! misbehaving, and the ceiling it would protect is the one they are
//! protecting.
//!
//! The refusal is ledgered. A 403 nobody can audit is indistinguishable from
//! a bug (constitution X), so the decision is minted by the kernel's own
//! denial path and its id goes out in front of the message the client reads.
//!
//! Spec 022's `RequireRole` is the same gate one crate over, and this is not
//! that type: spec 020 AC-2 keeps `rahi-idp` out of this crate's dependency
//! tree, so the edge reads the [`Principal`] the identity crate left in the
//! request extensions and asks the kernel it already holds (spec 024 D-2).

use axum::Router;
use axum::extract::{Request, State};
use axum::middleware::{Next, from_fn_with_state};
use axum::response::Response;
use rahi_kernel::Kernel;
use rahi_types::{Error, Principal, Role};
use serde_json::json;

use crate::error;

/// The decision kind an operator refusal is recorded under.
pub const DECISION_OPERATOR_DENIED: &str = "edge.operator";

/// The role gate every operator surface sits behind.
///
/// Built from the kernel, because the role is the manifest's and the manifest
/// is the kernel's: there is no second place to configure it and therefore no
/// way for a cell to gate its operator routes on a role its ceiling never
/// declared.
#[derive(Clone)]
pub struct RequireOperator {
    role: Role,
    kernel: Kernel,
}

impl std::fmt::Debug for RequireOperator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RequireOperator")
            .field("role", &self.role)
            .finish_non_exhaustive()
    }
}

impl RequireOperator {
    /// The gate over `kernel`'s `auth.operator_role`.
    #[must_use]
    pub fn new(kernel: Kernel) -> Self {
        let role = Role::new(kernel.manifest().auth.operator_role.clone());
        Self { role, kernel }
    }

    /// The role this gate requires.
    #[must_use]
    pub const fn role(&self) -> &Role {
        &self.role
    }
}

/// Wrap `router`'s routes in the operator gate.
///
/// `route_layer`, not `layer`: the gate runs only where a route matched, so a
/// path nobody mounted stays a 404 instead of becoming a 401. A gate that
/// answers the fallback tells every caller that everything exists, and it
/// does it on the paths that do not.
///
/// A router with no routes is returned untouched; there is nothing to gate,
/// and `route_layer` refuses that case outright.
pub fn with_operator<S>(require: RequireOperator, router: Router<S>) -> Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    if router.has_routes() {
        router.route_layer(from_fn_with_state(require, enforce))
    } else {
        router
    }
}

/// Answer 403 with a ledgered decision when the principal is not an operator.
///
/// A request with no principal is 401 rather than 403, the same distinction
/// spec 022 draws: "you are not who you say" and "you are, and it is not
/// enough" are answered by different actions on the client's side.
pub async fn enforce(
    State(require): State<RequireOperator>,
    request: Request,
    next: Next,
) -> Response {
    let Some(principal) = request.extensions().get::<Principal>().cloned() else {
        return error::response(&Error::Unauthorized(
            "this request carries no session".to_owned(),
        ));
    };
    if principal.has_role(&require.role) {
        return next.run(request).await;
    }

    let mut payload = serde_json::Map::new();
    payload.insert("role".to_owned(), json!(require.role.as_str()));
    payload.insert("path".to_owned(), json!(request.uri().path()));
    let reason = format!(
        "the principal does not hold the operator role {:?} this route requires",
        require.role.as_str()
    );
    let id = require.kernel.refuse(
        DECISION_OPERATOR_DENIED,
        &principal.sub,
        reason.clone(),
        payload,
    );

    error::response(&Error::Denied(format!("{id}: {reason}")))
}
