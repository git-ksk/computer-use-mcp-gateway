# V2 execution-budget acceptance

Date: 2026-09-20

Issue: #319

## Contract under test

The Cua-backed effectful surface must reject duration-bearing commands before backend dispatch when their conservative semantic duration cannot fit the configured bounded backend tool deadline. A real timeout after dispatch remains Indeterminate/no-replay. Signed payload-free timeout cause evidence may improve the durable quarantine reason but cannot claim completion or authorize replay.

Canonical design: [../V2_EXECUTION_BUDGET.md](../V2_EXECUTION_BUDGET.md)

## Automated evidence

The implementation covers:

- effective backend budget reservation and below/equal/above boundary semantics;
- Unicode scalar timing rather than UTF-8 byte timing for paced TypeText;
- the 2026-09-20 incident shape (1403 scalars, 30ms pacing, 30s backend timeout), rejected before any backend connection/tool dispatch;
- ordinary short typing admitted under the default budget;
- explicit audit coverage for TypeText, TypeTextAdvanced, PointerDrag, PointerDragAdvanced, VerifyUiState, and VerifyUiStateContextual;
- Process/Shell excluded from the Cua semantic budget because their supervised executor owns timeout semantics independently;
- impossible TypeText timing rejected before dispatch rather than converted into post-dispatch ambiguity;
- genuine post-dispatch browser backend timeout retaining TimedOutIndeterminate behavior;
- typed northbound execution_budget_exceeded without raw payload leakage;
- signed RemoteIndeterminateAck tamper rejection and payload-free encoding;
- Hub durable quarantine reason BackendTimedOut when signed timeout cause evidence arrives.

## Trusted physical macOS acceptance

Command:

    CUMG_V2_EXECUTION_BUDGET_E2E_ACK=1     CUMG_V2_CUA_COMMAND=/Users/sawadakousuke/.local/bin/cua-driver     cargo test --locked       v2_m1_backend::tests::real_cua_paced_type_text_execution_budget_acceptance       --lib -- --ignored --exact --nocapture

Environment:

- trusted physical macOS host;
- installed Cua Driver v0.19.3;
- harmless Calculator target;
- exact Calculator window foreground delivery;
- text: six numeric characters;
- delay_ms: 60;
- CUMG Cua tool timeout: 30 seconds;
- computed conservative required budget: 2528ms;
- effective semantic budget: 24000ms.

Result: PASS.

The acceptance verified all of the following in one real-Cua run:

1. the command was admitted by the semantic budget;
2. Cua returned successful TypeText completion;
3. elapsed time exceeded the test's 250ms lower bound, exercising the delay-bearing foreground synthesis path rather than an effectively instantaneous semantic write;
4. the Calculator window screenshot changed after the paced input;
5. the Calculator process was terminated after the observation and the interaction session/backend were cleanly closed.

Calculator does not reliably expose its displayed digits as AX label/value text in this environment, so accessibility text equality is not part of the acceptance oracle. The proof is deliberately limited to successful exact-window paced delivery, observed pacing, and post-input visual change. It does not widen the semantic guarantees of Cua's TypeText result.

## Release-boundary note

#319 introduces an additional signed Hub-Agent outer wire variant for payload-free timeout cause evidence. The final HUB_AGENT_SCHEMA_VERSION bump, mixed-version fail-closed coverage, readiness/config integration, packaged configuration, and v0.5 release candidate acceptance remain owned by #314.
