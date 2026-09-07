//! The governed facades (spec 015 B-5).
//!
//! A facade is the only door. It holds one capability's triple, adjudicates
//! every call against it, performs the operation on `Allow`, and on anything
//! else returns [`Error::Denied`] carrying the id of the decision the chain
//! will hold. The wrapped value is never handed out: [`Governed::call`] lends
//! it to a closure and only after the gate has said yes, so there is no
//! sequence of public calls on this type that reaches the store without
//! passing the kernel first.
//!
//! One facade is one capability, not one resource kind. A service that both
//! reads and writes `notes` builds two:
//!
//! ```no_run
//! # use rahi_kernel::{CapabilityKind, Governed, Kernel};
//! # use rahi_types::Sub;
//! # async fn f(kernel: &Kernel, store: rahi_store::StoreHandle) -> Result<(), rahi_types::Error> {
//! let reader = Governed::new(kernel, "notes", CapabilityKind::DbRead, "notes", store.clone())?;
//! let writer = Governed::new(kernel, "notes", CapabilityKind::DbWrite, "notes", store)?;
//! let actor = Sub::new("rauthy-subject");
//! let _rows: Vec<(String,)> = reader.query(&actor, "SELECT id FROM notes", vec![]).await?;
//! writer.execute(&actor, "DELETE FROM notes WHERE id = ?", vec!["1".into()]).await?;
//! # Ok(())
//! # }
//! ```
//!
//! That shape is what makes spec 015 B-3's build step possible: the triple is
//! three literals at the construction site, so the verifier reads what the
//! runtime will enforce rather than guessing at it.

/// The span a governed operation opens, which spec 023's layer turns into
/// `store_ops_total` and `store_op_duration_seconds` (spec 023 B-2).
///
/// The name and the field below are the whole contract between this crate and
/// the one that observes it: the kernel imports nothing from the edge (spec
/// 023 B-4), so what they share is a span name.
pub const STORE_SPAN: &str = "store.op";
/// The field on [`STORE_SPAN`] naming the capability kind, which is the label
/// the store metrics are counted under.
pub const STORE_SPAN_KIND: &str = "kind";

use std::fmt;
use std::future::Future;
use std::sync::Arc;

use rahi_store::{
    Envelope, ExecuteResult, Lease, Listen, Migration, MigrationReport, Notify, Statement,
    StoreHandle, Value,
};
use rahi_types::{Error, FenceToken, Sub};
use serde::Serialize;
use serde::de::DeserializeOwned;
use tracing::Instrument as _;

use crate::Kernel;
use crate::adjudicate::Request;
use crate::capability::{CapabilityKind, ServiceName};

/// Where a `Governed<Secrets>` reads from.
///
/// The chassis has no secret store of its own: the value comes from the
/// process environment, a mounted file, or whatever spec 031's packaging
/// arranges. What the kernel governs is *which names* a service may ask for.
pub trait SecretSource: Send + Sync {
    /// The value of `name`, or `None` when it is not set.
    fn read(&self, name: &str) -> Option<String>;
}

impl<F> SecretSource for F
where
    F: Fn(&str) -> Option<String> + Send + Sync,
{
    fn read(&self, name: &str) -> Option<String> {
        self(name)
    }
}

/// The secret half of the cell, behind a [`SecretSource`].
#[derive(Clone)]
pub struct Secrets(Arc<dyn SecretSource>);

impl Secrets {
    /// Wrap a source.
    #[must_use]
    pub fn new(source: Arc<dyn SecretSource>) -> Self {
        Self(source)
    }
}

impl fmt::Debug for Secrets {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Secrets").finish_non_exhaustive()
    }
}

/// The egress half of the cell.
///
/// A marker, because the chassis ships no HTTP client: what a
/// `Governed<Egress>` gives out is a [`Permit`], and the caller performs the
/// request with whatever client it carries. The governance question is which
/// hosts a service may reach, and that is answerable without owning the
/// transport.
#[derive(Clone, Copy, Debug, Default)]
pub struct Egress;

/// Permission to reach one host, from [`Governed::permit`].
///
/// Holding one means the kernel admitted the host under a declared
/// `http.egress` capability. It is not a connection and it does not expire;
/// it is the evidence a caller shows its own client.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Permit {
    host: String,
}

impl Permit {
    /// The admitted host.
    #[must_use]
    pub fn host(&self) -> &str {
        &self.host
    }
}

/// One capability, wrapped around the thing that performs it.
///
/// Construct with [`Governed::new`], naming the service, the kind, and the
/// resource as literals so spec 015 B-3's build step can read them.
pub struct Governed<T> {
    kernel: Kernel,
    service: ServiceName,
    kind: CapabilityKind,
    resource: String,
    inner: T,
}

impl<T> fmt::Debug for Governed<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Governed")
            .field("service", &self.service)
            .field("kind", &self.kind)
            .field("resource", &self.resource)
            .finish_non_exhaustive()
    }
}

impl<T> Clone for Governed<T>
where
    T: Clone,
{
    fn clone(&self) -> Self {
        Self {
            kernel: self.kernel.clone(),
            service: self.service.clone(),
            kind: self.kind,
            resource: self.resource.clone(),
            inner: self.inner.clone(),
        }
    }
}

impl<T> Governed<T> {
    /// Wrap `inner` as one capability of one service.
    ///
    /// Whether a grant covers the triple is not decided here: it is decided
    /// on every call, so every refusal takes the one path that ledgers it.
    /// What is decided here is whether the manifest declares the service at
    /// all, because a facade for a service nobody declared is a typo rather
    /// than a policy question.
    ///
    /// # Errors
    ///
    /// [`Error::Validation`] when `service` is not a well-formed name or is
    /// not declared in the manifest.
    pub fn new(
        kernel: &Kernel,
        service: &str,
        kind: CapabilityKind,
        resource: &str,
        inner: T,
    ) -> Result<Self, Error> {
        let service = ServiceName::parse(service)?;
        if !kernel.manifest().services.contains_key(&service) {
            return Err(Error::Validation(format!(
                "the manifest declares no service {service}; a facade names a service the \
                 manifest grants to"
            )));
        }
        Ok(Self {
            kernel: kernel.clone(),
            service,
            kind,
            resource: resource.to_owned(),
            inner,
        })
    }

    /// The service this facade is attributed to.
    #[must_use]
    pub fn service(&self) -> &ServiceName {
        &self.service
    }

    /// The capability kind this facade was declared for.
    #[must_use]
    pub fn kind(&self) -> CapabilityKind {
        self.kind
    }

    /// The resource this facade addresses.
    #[must_use]
    pub fn resource(&self) -> &str {
        &self.resource
    }

    /// The kernel that adjudicates it.
    #[must_use]
    pub fn kernel(&self) -> &Kernel {
        &self.kernel
    }

    /// A request for this facade's triple, on behalf of `actor`.
    #[must_use]
    pub fn request(&self, actor: &Sub) -> Request {
        Request::new(
            self.service.clone(),
            self.kind,
            self.resource.clone(),
            actor.clone(),
        )
    }

    /// Adjudicate `request`, then run `op` on the wrapped value.
    ///
    /// The seam every governed operation goes through. `op` receives the
    /// wrapped value only after the gate returned `Allow`.
    ///
    /// # Errors
    ///
    /// [`Error::Validation`] when `request` is not for this facade's triple;
    /// [`Error::Denied`] carrying the decision id when the gate refuses;
    /// otherwise whatever `op` returns.
    pub async fn call<'a, F, Fut, R>(&'a self, request: Request, op: F) -> Result<R, Error>
    where
        F: FnOnce(&'a T) -> Fut,
        Fut: Future<Output = Result<R, Error>>,
    {
        if request.service != self.service
            || request.kind != self.kind
            || request.resource != self.resource
        {
            return Err(Error::Validation(format!(
                "this facade governs ({}, {}, {}) and was handed a request for ({}, {}, {})",
                self.service,
                self.kind,
                self.resource,
                request.service,
                request.kind,
                request.resource
            )));
        }
        // Spec 023 B-2: one span per governed store operation. The kernel
        // knows nothing about who is listening; `tracing` is the seam, and
        // spec 023's layer turns a closed span into the store metrics.
        let span = tracing::info_span!(
            STORE_SPAN,
            kind = request.kind.as_str(),
            service = request.service.as_str(),
            resource = %request.resource,
        );
        async move {
            self.kernel.admit(&request).await?;
            op(&self.inner).await
        }
        .instrument(span)
        .await
    }

    /// Refuse a call whose kind is not the one this facade was declared for.
    fn expect(&self, kind: CapabilityKind) -> Result<(), Error> {
        if self.kind == kind {
            return Ok(());
        }
        Err(Error::Validation(format!(
            "this facade was declared for {} and cannot perform {kind}; one facade is one \
             capability",
            self.kind
        )))
    }
}

impl Governed<StoreHandle> {
    /// Read the local replica (`db.read`).
    ///
    /// # Errors
    ///
    /// As [`Governed::call`], then [`StoreHandle::query`].
    pub async fn query<R>(
        &self,
        actor: &Sub,
        sql: &'static str,
        values: Vec<Value>,
    ) -> Result<Vec<R>, Error>
    where
        R: DeserializeOwned + Send + 'static,
    {
        self.expect(CapabilityKind::DbRead)?;
        let request = self.request(actor).with_table(self.resource.clone());
        self.call(request, |store| store.query(sql, values)).await
    }

    /// Read through the leader (`db.read`).
    ///
    /// # Errors
    ///
    /// As [`Governed::call`], then [`StoreHandle::query_consistent`].
    pub async fn query_consistent<R>(
        &self,
        actor: &Sub,
        sql: &'static str,
        values: Vec<Value>,
    ) -> Result<Vec<R>, Error>
    where
        R: DeserializeOwned + Send + 'static,
    {
        self.expect(CapabilityKind::DbRead)?;
        let request = self.request(actor).with_table(self.resource.clone());
        self.call(request, |store| store.query_consistent(sql, values))
            .await
    }

    /// One statement, one write (`db.write`).
    ///
    /// # Errors
    ///
    /// As [`Governed::call`], then [`StoreHandle::execute`].
    pub async fn execute(
        &self,
        actor: &Sub,
        sql: &'static str,
        values: Vec<Value>,
    ) -> Result<ExecuteResult, Error> {
        self.expect(CapabilityKind::DbWrite)?;
        let request = self.request(actor).with_table(self.resource.clone());
        self.call(request, |store| store.execute(sql, values)).await
    }

    /// A batch as one transaction (`db.txn`).
    ///
    /// # Errors
    ///
    /// As [`Governed::call`], then [`StoreHandle::txn`].
    pub async fn txn(
        &self,
        actor: &Sub,
        statements: Vec<Statement>,
    ) -> Result<Vec<ExecuteResult>, Error> {
        self.expect(CapabilityKind::DbTxn)?;
        let request = self.request(actor).with_table(self.resource.clone());
        self.call(request, |store| store.txn(statements)).await
    }

    /// Apply versioned DDL (`db.migrate`).
    ///
    /// # Errors
    ///
    /// As [`Governed::call`], then [`StoreHandle::migrate`].
    pub async fn migrate(
        &self,
        actor: &Sub,
        migrations: Vec<Migration>,
    ) -> Result<MigrationReport, Error> {
        self.expect(CapabilityKind::DbMigrate)?;
        let request = self.request(actor).with_table(self.resource.clone());
        self.call(request, move |store| async move {
            store.migrate(&migrations).await
        })
        .await
    }

    /// Read a cache key (`kv.get`).
    ///
    /// # Errors
    ///
    /// As [`Governed::call`], then [`StoreHandle::kv_get`].
    pub async fn kv_get<V>(&self, actor: &Sub, key: &str) -> Result<Option<V>, Error>
    where
        V: DeserializeOwned,
    {
        self.expect(CapabilityKind::KvGet)?;
        let request = self.request(actor).with_key(key);
        self.call(request, |store| store.kv_get(key)).await
    }

    /// Write a cache key (`kv.put`).
    ///
    /// # Errors
    ///
    /// As [`Governed::call`], then [`StoreHandle::kv_put`].
    pub async fn kv_put<V>(
        &self,
        actor: &Sub,
        key: &str,
        value: &V,
        ttl_secs: Option<u32>,
    ) -> Result<(), Error>
    where
        V: Serialize + Sync,
    {
        self.expect(CapabilityKind::KvPut)?;
        let request = self.request(actor).with_key(key);
        self.call(request, |store| store.kv_put(key, value, ttl_secs))
            .await
    }

    /// Delete a cache key (`kv.delete`).
    ///
    /// # Errors
    ///
    /// As [`Governed::call`], then [`StoreHandle::kv_del`].
    pub async fn kv_del(&self, actor: &Sub, key: &str) -> Result<(), Error> {
        self.expect(CapabilityKind::KvDelete)?;
        let request = self.request(actor).with_key(key);
        self.call(request, |store| store.kv_del(key)).await
    }

    /// Read a counter (`counter.get`).
    ///
    /// # Errors
    ///
    /// As [`Governed::call`], then [`StoreHandle::counter_get`].
    pub async fn counter_get(&self, actor: &Sub, key: &str) -> Result<Option<i64>, Error> {
        self.expect(CapabilityKind::CounterGet)?;
        let request = self.request(actor).with_key(key);
        self.call(request, |store| store.counter_get(key)).await
    }

    /// Add to a counter (`counter.add`).
    ///
    /// # Errors
    ///
    /// As [`Governed::call`], then [`StoreHandle::counter_add`].
    pub async fn counter_add(&self, actor: &Sub, key: &str, by: i64) -> Result<i64, Error> {
        self.expect(CapabilityKind::CounterAdd)?;
        let request = self.request(actor).with_key(key);
        self.call(request, |store| store.counter_add(key, by)).await
    }

    /// Take a fenced lease (`lock.acquire`).
    ///
    /// The lease comes back already governed, so the writes it guards are
    /// adjudicated too.
    ///
    /// # Errors
    ///
    /// As [`Governed::call`], then [`StoreHandle::lease`].
    pub async fn lease(&self, actor: &Sub, key: &str) -> Result<Governed<Lease>, Error> {
        self.expect(CapabilityKind::LockAcquire)?;
        let request = self.request(actor).with_key(key);
        let lease = self.call(request, |store| store.lease(key)).await?;
        Ok(Governed {
            kernel: self.kernel.clone(),
            service: self.service.clone(),
            kind: CapabilityKind::LockAcquire,
            resource: self.resource.clone(),
            inner: lease,
        })
    }
}

impl Governed<Lease> {
    /// The key this lease holds.
    #[must_use]
    pub fn key(&self) -> &str {
        self.inner.key()
    }

    /// The fencing token the guarded writes carry.
    #[must_use]
    pub fn token(&self) -> FenceToken {
        self.inner.token
    }

    /// A batch guarded by this lease's fence (`db.txn` on `table`).
    ///
    /// Adjudicated as a transaction on `table`, not as the lock: the lock was
    /// already granted, and what needs a grant now is the write.
    ///
    /// # Errors
    ///
    /// [`Error::Denied`] when no grant covers `db.txn` on `table`; otherwise
    /// as [`StoreHandle::fenced_txn`], including [`Error::Conflict`] when the
    /// lease has been superseded.
    pub async fn fenced_txn(
        &self,
        actor: &Sub,
        table: &str,
        statements: Vec<Statement>,
    ) -> Result<Vec<ExecuteResult>, Error> {
        let request = Request::new(
            self.service.clone(),
            CapabilityKind::DbTxn,
            table.to_owned(),
            actor.clone(),
        )
        .with_table(table)
        .with_key(self.inner.key());
        let span = tracing::info_span!(
            STORE_SPAN,
            kind = request.kind.as_str(),
            service = request.service.as_str(),
            resource = %request.resource,
        );
        async move {
            self.kernel.admit(&request).await?;
            self.kernel
                .store_handle()
                .fenced_txn(&self.inner, statements)
                .await
        }
        .instrument(span)
        .await
    }

    /// Hand the lease back.
    pub async fn release(self) {
        self.inner.release().await;
    }
}

impl Governed<Notify> {
    /// Publish an envelope on this facade's topic (`notify.publish`).
    ///
    /// The topic is the facade's resource and the envelope's `kind`; an
    /// envelope for another kind is refused before the gate is asked, because
    /// it is a mistake rather than a denial.
    ///
    /// # Errors
    ///
    /// [`Error::Validation`] when the envelope's kind is not this facade's
    /// topic; otherwise as [`Governed::call`], then [`Notify::notify`].
    pub async fn publish(&self, actor: &Sub, envelope: Envelope) -> Result<(), Error> {
        self.expect(CapabilityKind::NotifyPublish)?;
        if envelope.kind != self.resource {
            return Err(Error::Validation(format!(
                "this facade publishes {:?} and was handed an envelope of kind {:?}",
                self.resource, envelope.kind
            )));
        }
        let request = self.request(actor).with_key(envelope.name.clone());
        self.call(request, move |notify| notify.notify(envelope))
            .await
    }

    /// Subscribe to this node's envelopes (`notify.listen`).
    ///
    /// The stream is not filtered to the topic: hiqlite publishes one stream
    /// per node and filtering is the consumer's, as it is in spec 012. What
    /// the grant governs is whether this service may listen at all.
    ///
    /// # Errors
    ///
    /// As [`Governed::call`].
    pub async fn listen(&self, actor: &Sub) -> Result<Listen, Error> {
        self.expect(CapabilityKind::NotifyListen)?;
        let request = self.request(actor);
        self.call(request, |notify| async { Ok(notify.listen()) })
            .await
    }
}

impl Governed<Secrets> {
    /// Read this facade's secret (`secret.read`).
    ///
    /// The name is the facade's resource, so the grant and the lookup cannot
    /// disagree.
    ///
    /// # Errors
    ///
    /// [`Error::Denied`] when no grant covers the name; [`Error::NotFound`]
    /// when the source has no value for it.
    pub async fn read(&self, actor: &Sub) -> Result<String, Error> {
        self.expect(CapabilityKind::SecretRead)?;
        let request = self.request(actor);
        let name = self.resource.clone();
        self.call(request, move |secrets| {
            let value = secrets.0.read(&name);
            async move {
                value.ok_or_else(|| Error::NotFound(format!("secret {name:?} is not set")))
            }
        })
        .await
    }
}

impl Governed<Egress> {
    /// Admit one host under this facade's `http.egress` capability.
    ///
    /// # Errors
    ///
    /// [`Error::Denied`] when no grant covers the host.
    pub async fn permit(&self, actor: &Sub, host: &str) -> Result<Permit, Error> {
        self.expect(CapabilityKind::HttpEgress)?;
        let request = Request::new(
            self.service.clone(),
            CapabilityKind::HttpEgress,
            host.to_owned(),
            actor.clone(),
        )
        .with_host(host);
        self.kernel.admit(&request).await?;
        Ok(Permit {
            host: host.to_owned(),
        })
    }
}
