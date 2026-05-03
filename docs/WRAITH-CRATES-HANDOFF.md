# Wraith Crates — ClaudioOS Integration Handoff

**Author:** wraith-browser session, 2026-05-02
**For:** the next ClaudioOS session
**Repos involved:** `J:\wraith-browser\` (this is the wraith side) and `J:\baremetal claude\` (you).

---

## Status as of 2026-05-02

The 3 bare-metal crates that the wraith-browser handoff (2026-04-02) named —
`wraith-dom`, `wraith-transport`, `wraith-render` — exist at
`J:\baremetal claude\crates\` and **all compile clean** (`cargo check -p
wraith-dom -p wraith-transport -p wraith-render` from the ClaudioOS workspace
root succeeds — only unused-helper warnings, no errors).

But there's a **hole that blocks actual end-to-end integration**, and that's
what this doc is about.

## The hole: `wraith-transport` (here) doesn't implement wraith-browser's `HttpTransport` trait

Look at `crates/wraith-transport/src/lib.rs` line 3:

> "This crate bridges wraith-browser's `HttpTransport` trait into ClaudioOS's
> bare-metal network stack."

That's the design intent. But the actual code:

- Defines its own `pub struct SmoltcpTransport { ... }` (line 176)
- Defines a private `fn execute_sync(...)` (line 222)
- **Never imports `HttpTransport`**
- **Never writes `impl HttpTransport for SmoltcpTransport`**
- `Cargo.toml` has zero dependency on the wraith-browser-side `wraith-transport` crate

So the trait that the wraith-browser engine asks for (`Arc<dyn
wraith_transport::HttpTransport>`) is never produced by this crate. The kernel
cannot wire `SmoltcpTransport` into a `SevroEngine` today — the types are
unrelated despite the matching crate name.

## What just shipped on the wraith-browser side (FR-1, 2026-05-02)

The wraith-browser side is now ready to receive a transport:

- `transport: Option<Arc<dyn HttpTransport>>` field on `SevroEngine`, available
  in **both** std and no-std builds.
- New constructor `SevroEngine::with_transport(config, transport)` — works in
  both modes; this is the kernel's entry point.
- New helper `SevroEngine::fetch(url) -> Result<String, String>` — uses only
  the trait (no reqwest, no QuickJS, no DOM parse).
- Both `cargo check -p sevro-headless` and `cargo check -p sevro-headless
  --no-default-features` pass.

So as soon as ClaudioOS produces an `Arc<dyn HttpTransport>`, the kernel can
do:

```rust
let engine = SevroEngine::with_transport(config, my_smoltcp_transport);
let html  = engine.fetch("https://example.com").await?;
```

…and the engine's no_std path is alive.

## The cross-repo dependency choice (this is the real decision)

`HttpTransport` is defined in `J:\wraith-browser\crates\transport\src\lib.rs`
under crate name `wraith-transport`. That crate is **not published to
crates.io**. To get the trait into the ClaudioOS workspace, you have to pick
one of:

**Option A — path-dep across the two repos** (cheapest now, fragile later)

```toml
# In J:\baremetal claude\crates\wraith-transport\Cargo.toml
[dependencies]
wraith-browser-transport = { path = "../../../wraith-browser/crates/transport", package = "wraith-transport" }
```

(Uses `package = "wraith-transport"` to dodge the name collision with the
ClaudioOS-side homonym crate.)

- ✅ No publishing, no vendoring, immediate.
- ❌ Breaks if the two repo dirs are not siblings on every machine.
- ❌ Couples the ClaudioOS workspace's `cargo check` to the wraith-browser
  source tree being present.

**Option B — vendor a copy of the trait** (clean for now, drift later)

Copy `wraith-browser/crates/transport/src/lib.rs` into ClaudioOS as
`crates/wraith-trait/src/lib.rs`, depend on it. Both `wraith-transport` (here)
and the wraith-browser engine then implement / consume the same trait
*definition*. Trait stability is on you — drift between the two copies will
silently break linking when the wraith-browser side actually links against
both crates (which currently it does not — the wraith-browser engine is std,
ClaudioOS is no_std, they don't co-link in the same binary). So drift is
mostly cosmetic until someone tries to literally pass an Arc across.

- ✅ No path-dep weirdness, ClaudioOS workspace is self-contained.
- ❌ Two copies of a trait definition that have to stay in sync.

**Option C — publish `wraith-transport` from wraith-browser to crates.io**
(cleanest long-term, real work now)

Rename it (the wraith-browser-side one) to something less generic
(`wraith-browser-transport`?), publish 0.1, both sides depend on it.

- ✅ One source of truth.
- ❌ Publishing a crate locks you into semver, requires a maintenance cadence,
  and crates.io is forever — you can't unpublish, only yank.

**Recommendation:** Option B for the next 30 days. Once the bare-metal port is
demonstrated end-to-end and stable, escalate to C. Skip A — the repo-sibling
assumption will bite the moment you try to build ClaudioOS in a CI container or
on a fresh checkout.

## The work, concretely (steps the ClaudioOS session should do)

1. **Pick A/B/C** above. Default to B unless there's a reason not to.
2. **Implement the bridge** in `crates/wraith-transport/src/lib.rs`:

   ```rust
   use alloc::sync::Arc;
   use wraith_trait::{HttpTransport, TransportRequest, TransportResponse, TransportError, TransportMethod};

   #[async_trait::async_trait]    // also need alloc-flavored async-trait
   impl HttpTransport for SmoltcpTransport {
       async fn execute(&self, request: TransportRequest) -> Result<TransportResponse, TransportError> {
           let method = match request.method {
               TransportMethod::Get => "GET",
               TransportMethod::Post => "POST",
           };
           let resp = self.execute_sync(method, &request.url, &request.headers, request.body.as_deref())
               .map_err(|e| match e {
                   SmoltcpTransportError::NoNetwork    => TransportError::ConnectionFailed("network not ready".into()),
                   SmoltcpTransportError::TcpError(e)  => TransportError::ConnectionFailed(format!("{e:?}")),
                   SmoltcpTransportError::TlsError(e)  => TransportError::TlsError(format!("{e:?}")),
                   SmoltcpTransportError::Timeout      => TransportError::Timeout,
                   other                               => TransportError::Other(format!("{other}")),
               })?;
           Ok(TransportResponse {
               status: resp.status_code,
               headers: resp.headers,        // already a BTreeMap<String,String>
               body: resp.body,
               url: request.url,             // smoltcp side doesn't track redirects today
               set_cookie_headers: resp.set_cookie_headers.unwrap_or_default(),
           })
       }
   }
   ```

   Adjust field names to match what `SmoltcpResponse` actually exposes — I
   didn't read every line of it.

3. **`async-trait` in no_std:** the wraith-browser-side `wraith-transport`
   crate uses `async_trait::async_trait` with std. For no_std consumers you
   either:
   - Use `async-trait` with `default-features = false` (it's `no_std`-friendly
     since 0.1.74 with the `alloc` feature)
   - Or, if `async-trait`'s allocator requirements are too heavy for the
     kernel, switch the trait definition to RPITIT (`async fn` in trait,
     stable since 1.75) — which removes the `dyn` storage but the Wraith engine
     uses `Arc<dyn HttpTransport>` so RPITIT alone won't work for the dyn case.
     Best path: keep `async_trait` with `alloc` feature, depend on
     `extern crate alloc`.

4. **Smoke test** in the kernel — wire up:

   ```rust
   let smoltcp_transport: Arc<dyn HttpTransport> = Arc::new(unsafe {
       SmoltcpTransport::new(&mut KERNEL_NETWORK_STACK as *mut _, kernel_now, rng_seed)
   });
   let mut engine = SevroEngine::with_transport(SevroConfig::default(), smoltcp_transport);
   let html = engine.fetch("https://example.com").await?;
   log::info!("got {} bytes", html.len());
   ```

   If you don't have a sevro-headless dependency in the kernel yet, add it as
   a path-dep to `J:\wraith-browser\sevro\ports\headless\` (or vendor — same
   A/B/C choice as the trait crate).

5. **Verify** with QEMU + the existing networking smoke test. If `example.com`
   parses, the bridge is real.

## Don't bother with these (yet)

- **Wiring all 20+ direct `self.client` reqwest calls in
  `sevro-headless/src/lib.rs`** through the trait. Those are inside
  `#[cfg(feature = "std")]` methods so they don't touch the no_std build.
  Optional cleanup, not a blocker. The kernel only needs `engine.fetch(url)`
  to work; everything else (form fill, JS eval, etc.) requires capabilities
  the bare-metal port doesn't have anyway.
- **Re-implementing `wraith-render` paint paths.** They're done as far as
  this issue is concerned.
- **Bringing up smoltcp on the wraith-browser side.** Not the goal.

## Where the wraith-browser FR-4 is tracked

`J:\wraith-browser\NEXT-UP.md` — search for "FR-4". Mark it done over there
once the QEMU smoke test passes.

## Cross-refs

- `J:\wraith-browser\HANDOFF.md` (top, "Session Summary — 2026-05-01" + the
  earlier 2026-04-02 entry where the 3 crates were originally created)
- `J:\wraith-browser\sevro\ports\headless\src\lib.rs` line ~280 — `with_transport`, the entry point
- `J:\wraith-browser\crates\transport\src\lib.rs` — the trait definition you need to import or vendor
- `J:\baremetal claude\docs\WRAITH-BAREMETAL-PORT.md` — the older port-strategy doc (orthogonal to this — that one's about porting the engine; this one's about wiring the network stack into the already-ported engine)
