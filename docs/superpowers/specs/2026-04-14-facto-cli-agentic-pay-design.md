# Facto CLI Agentic Pay Design

## 1. Goal

Design a single `facto-cli` payment experience that works for both:

- terminal users
- headless/agent callers

The CLI must support agentic payments across both:

- `x402`
- `MPP`

without introducing separate top-level user flows such as `facto monad pay` or `facto tempo pay`.

The command surface should remain centered on:

- `facto pay`
- `facto balance`
- `facto fund`
- `facto pipelines`
- `facto pipeline create`

## 2. Scope And Non-Goals

### In scope

- `facto pay` protocol auto-detection and protocol override flags
- pipeline selection behavior for agentic payments
- distinction between persistent default pipeline and per-payment execution pipeline
- CLI behavior for human terminal use and agent/headless use
- CLI output contract for human-readable and terse/JSON modes
- CLI-triggered onboarding for missing compatible pipelines
- backend API requirements needed to support the CLI design
- local and end-to-end test strategy

### Out of scope

- changing card/mainstream payment product flows
- redesigning pipeline data model in the backend
- introducing protocol-specific top-level commands
- implementing payment methods directly in the CLI
- replacing the backend as the source of truth for protocol/method capability

## 3. Current Context

`facto-cli` today already revolves around a single selected pipeline:

- local config stores `default_pipeline_id`
- several commands resolve chain context from the selected route
- pipeline listing and selection already exist

However, the current payment path is still x402-specific:

- `facto pay` posts to `/v1/x402/pay`
- onboarding and prompts are Base-first
- there is no shared payment preflight/resolve flow

In parallel, `facto-aa` now has an MPP agentic backend path for `monad/charge`, with local testing already verified. The CLI therefore no longer needs to invent protocol rules on its own; it needs a backend-driven orchestration contract.

## 4. User Models

This design explicitly supports two modes.

### 4.1 Terminal user

Characteristics:

- interactive
- can confirm funding actions
- can be guided to browser flows
- expects concise but understandable output

### 4.2 Agent/headless caller

Characteristics:

- usually runs with `-t` / terse JSON output
- must not depend on browser popups
- prefers deterministic, structured errors and machine-readable guidance
- cares more about successful execution than about preserving conversational UX

The same command semantics should apply to both modes. The difference is only in how the CLI handles ambiguity, confirmation, and remediation.

## 5. Key Terms

### 5.1 Selected pipeline

The persistent, user-chosen pipeline stored as `default_pipeline_id`.

This remains the user's durable context. It is what:

- `facto balance` reads
- `facto fund` defaults to
- `facto pipelines default <id>` changes

### 5.2 Execution pipeline

The pipeline actually used for a specific `facto pay` invocation.

This may equal the selected pipeline, but it may also be a temporary compatible pipeline chosen only for the current payment.

The CLI must never silently rewrite `default_pipeline_id` just because it used a different execution pipeline for one payment.

### 5.3 Protocol auto

The default `facto pay` mode. The CLI asks the backend what kind of payment the target URL requires, rather than assuming `x402`.

### 5.4 Compatible pipeline

A pipeline that matches the backend-declared payment requirement for the target service, including the needed:

- protocol family
- chain
- asset/currency
- method-specific constraints

## 6. Design Principles

1. One top-level payment command.
2. The backend decides protocol and method capability; the CLI orchestrates.
3. The selected pipeline is persistent; the execution pipeline is transient.
4. Automatic selection is allowed for a single obvious compatible pipeline.
5. Automatic funding uses the execution pipeline, not an arbitrary other pipeline.
6. Human UX and agent UX share logic but differ in remediation behavior.
7. The CLI should be able to ship once and remain future-compatible as backend payment methods expand.

## 7. Alternatives Considered

### Option A: keep `facto pay` x402-first, add `--protocol mpp`

Pros:

- smallest short-term change
- least disruption

Cons:

- not aligned with the intended long-term product model
- duplicates migration work later
- still makes the CLI protocol-aware in the wrong place

### Option B: make `facto pay` fully protocol-auto now

Pros:

- matches the intended product experience
- simplest user mental model

Cons:

- requires a stronger backend preflight contract immediately
- larger one-shot change

### Option C: full end-state architecture with phased implementation

Pros:

- clean architecture
- easier rollout sequencing

Cons:

- the CLI would still need another visible behavior change later

### Decision

Choose **Option B** as the target design and deliver it in one release, with backend and CLI changes landing together.

This matches the desired user experience:

- `facto pay` should decide for the user
- `--protocol mpp` and `--protocol x402` remain available as explicit overrides
- future methods such as `tempo` should not require another CLI redesign

## 8. User Flow Overview

### 8.1 Default payment flow

1. User runs `facto pay <method> <url>`.
2. CLI loads auth and the current selected pipeline.
3. CLI calls a backend payment preflight/resolve endpoint.
4. Backend returns payment requirements and compatible pipelines.
5. CLI chooses an execution pipeline.
6. CLI checks whether funding is needed for that execution pipeline.
7. CLI optionally funds.
8. CLI executes the payment against the correct backend pay endpoint.
9. CLI prints a unified result, including both selected and execution pipeline context.

### 8.2 Important behavior

- If the selected pipeline is compatible, it is used.
- If it is not compatible, but exactly one compatible pipeline exists, the CLI may use it temporarily for this payment.
- If multiple compatible pipelines exist, terminal mode asks the user; headless mode returns structured guidance unless an unambiguous backend recommendation exists.
- The selected pipeline remains unchanged unless the user explicitly changes it.

## 9. Command Design

### 9.1 `facto pay`

New default behavior:

```bash
facto pay GET <url>
```

Defaults to:

```text
--protocol auto
```

Supported overrides:

```bash
facto pay GET <url> --protocol auto
facto pay GET <url> --protocol x402
facto pay GET <url> --protocol mpp
```

Behavior:

- `auto`: use backend preflight to determine whether payment is `none`, `x402`, or `mpp`
- `x402`: force x402 path
- `mpp`: force MPP path

The CLI must not add method-specific top-level flags such as `--method monad` for the normal path. Method selection belongs to backend resolution.

### 9.2 `facto balance`

`facto balance` continues to show the balance for the persistent selected pipeline.

It must not switch to showing the execution pipeline from the last payment. This avoids user confusion and preserves the meaning of the selected pipeline.

If a payment used a non-default execution pipeline, the pay result should include that information explicitly instead of changing later command behavior.

### 9.3 `facto fund`

`facto fund` keeps its current meaning: funding the selected pipeline by default.

During `facto pay`, however, any automatic funding action must target the execution pipeline chosen for that payment.

### 9.4 `facto pipeline create`

Add two capabilities:

```bash
facto pipeline create --chain base|monad|tempo
facto pipeline create --for-pay <url>
```

Rules:

- `--chain` opens or guides creation for a specific chain
- `--for-pay <url>` first resolves the target service and then opens the creation flow best suited for that payment requirement
- plain `facto pipeline create` remains supported and opens the generic flow

This keeps the command stable while allowing payment-driven onboarding.

## 10. Pipeline Selection Rules

### 10.1 Selection algorithm

Given:

- selected pipeline
- backend-declared payment requirement
- compatible pipeline set

the CLI resolves the execution pipeline as follows:

1. If selected pipeline is compatible, use it.
2. Else if exactly one compatible pipeline exists, use it temporarily.
3. Else if multiple compatible pipelines exist:
   - terminal mode: prompt user to choose
   - headless mode: return structured guidance unless backend returns a single recommended pipeline
4. Else no compatible pipeline exists:
   - terminal mode: guide to create one
   - headless mode: return structured creation guidance

### 10.2 No silent default mutation

Temporary execution pipeline selection must not update `default_pipeline_id`.

If the CLI temporarily uses another pipeline, it should communicate that explicitly:

- terminal mode: human-readable message
- terse mode: structured fields such as `execution_pipeline_id` and `default_pipeline_unchanged=true`

### 10.3 Recommendation vs. mutation

The CLI may recommend that a user adopt a different pipeline as default, but it must never do so implicitly.

Explicit commands remain the only way to change the default:

```bash
facto pipelines default <id>
```

## 11. Funding Behavior

### 11.1 When funding is needed

If the execution pipeline is compatible but the corresponding agentic payment balance is insufficient, the CLI should attempt to fund before paying.

### 11.2 Funding policy by mode

- terminal mode: ask for confirmation before funding
- agent/headless mode: fund automatically

### 11.3 Funding scope

Automatic funding is tied to the execution pipeline only.

The CLI must not search arbitrary other chains or pipelines for money. It uses the funding context of the pipeline selected to satisfy the current payment requirement.

### 11.4 Funding visibility

The payment result should expose:

- whether funding occurred
- how much was funded
- which execution pipeline was funded
- the post-funding/post-payment balance summary if available

## 12. Backend Contract Required For One-Shot Delivery

To avoid hardcoding protocol logic into the CLI, the backend must expose a unified preflight endpoint.

Recommended endpoint:

```text
POST /v1/pay/resolve
```

This endpoint is required for the CLI design and is part of the same delivery.

### 12.1 Request

Input should include enough information to evaluate the payment target:

- URL
- HTTP method
- request headers
- request body
- optional protocol override
- optional max amount

### 12.2 Response

The response should include at minimum:

- `requires_payment`
- `protocol`: `none | x402 | mpp`
- `payment_method`: e.g. `monad`, `tempo`, or `null`
- `chain_id`
- `asset` / `currency`
- `quoted_amount`
- `compatible_pipeline_ids`
- `recommended_execution_pipeline_id`
- `can_auto_fund`
- `create_pipeline_url`
- `reason` / structured incompatibility details when no compatible pipeline exists

This keeps the CLI generic. As backend support expands from `monad` to `tempo` or beyond, the CLI should require little or no behavioral redesign.

## 13. Human UX

### 13.1 Successful temporary execution pipeline

Example:

```text
Using compatible Monad pipeline <id> for this payment.
Your default pipeline remains <base-id>.
```

### 13.2 Funding confirmation in terminal mode

Example:

```text
This payment requires funding on Monad via pipeline <id>.
Fund $0.25 USDC and continue? [y/N]
```

### 13.3 Missing compatible pipeline

Example:

```text
This service requires MPP on Monad.
No compatible pipeline is currently available.
Create one now: <url>
```

If the user invoked `facto pipeline create --for-pay <url>`, the CLI can skip the explanation and open the targeted creation flow directly.

## 14. Agent And Terse Output Contract

Terse mode should return a stable JSON envelope across both x402 and MPP as much as possible.

Recommended top-level fields:

- `status`
- `http_status`
- `protocol`
- `payment_method`
- `selected_pipeline_id`
- `execution_pipeline_id`
- `default_pipeline_unchanged`
- `funding_action`
- `payment`
- `response`
- `error`

### 14.1 Structured remediation

When the CLI cannot proceed in headless mode, it should return machine-usable guidance rather than browser-driven prose.

Recommended structured guidance fields:

- `required_protocol`
- `required_method`
- `required_chain_id`
- `compatible_pipeline_ids`
- `recommended_execution_pipeline_id`
- `create_pipeline_url`
- `suggested_next_action`

## 15. Testing Strategy

This design is intended for one-shot delivery, so test coverage must include CLI and backend integration together.

### 15.1 CLI unit tests

Cover:

- `protocol=auto` decision handling
- explicit `--protocol x402|mpp`
- selected vs execution pipeline behavior
- no silent default mutation
- single compatible pipeline auto-selection
- multiple compatible pipeline branching
- headless structured failure output
- terminal funding confirmation branching

### 15.2 Local CLI integration tests

Use:

- `FACTO_API_URL=http://localhost:8080`
- `facto login --dev-token ...`
- local `facto-aa`
- local MPP mock

Scenarios:

- selected Base pipeline, one Monad-compatible pipeline exists, payment succeeds via temporary execution pipeline
- selected pipeline compatible, payment succeeds directly
- no compatible pipeline, structured remediation returned

### 15.3 End-to-end funding scenarios

Cover:

- compatible pipeline with sufficient balance
- compatible pipeline requiring funding first
- terminal confirmation branch
- headless auto-fund branch

### 15.4 Regression coverage

Existing x402 smoke and history flows must continue to work. The CLI redesign must not break:

- current login flow
- selected pipeline persistence
- x402 terse output used by agents

## 16. Rollout Notes

This design assumes a single coordinated delivery across backend and CLI.

The minimum acceptable release set is:

- backend `pay/resolve` endpoint
- backend MPP pay endpoint
- CLI `protocol=auto`
- CLI execution-pipeline logic
- CLI targeted pipeline creation flow
- local and CI coverage for both x402 and MPP

Without the shared preflight contract, the CLI would be forced to re-encode backend payment logic and would become brittle as MPP methods expand.

## 17. Final Recommendation

Ship a single, backend-driven payment orchestration model in `facto-cli`:

- keep one persistent selected pipeline
- allow a temporary execution pipeline per payment
- default `facto pay` to `protocol=auto`
- keep `--protocol x402|mpp` as explicit overrides
- auto-use a single obvious compatible pipeline without mutating default state
- fund through the execution pipeline
- keep `facto balance` tied to the selected pipeline
- expose enough structured output for agent/headless use
- rely on backend preflight/resolve so future methods such as `tempo` do not require another CLI redesign

This produces the right user experience, the right agent experience, and the right long-term separation of responsibilities between CLI and backend.
