# Weekly Wheel Agent: Design Doc

| | |
|---|---|
| **Status** | Draft for review |
| **Date** | 2026-11-10 |
| **Scope** | Redesign of the Monday / Tue–Thu / Friday agent prototype |

This document describes system design. It is not financial advice; every threshold marked *proposed* is a starting value for you to set.

---

## 1. Summary

The prototype is three standalone TypeScript scripts plus three prompts. Its rules are sensible, but it has five structural problems:

1. It finds trades by scanning for premium and then filtering for quality, which selects the riskiest names first.
2. It evaluates each trade alone, so nothing limits the risk of the book as a whole.
3. The three scripts share no state, and the middle one overwrites position intent from scores that may be missing.
4. Several checks treat missing data as safe.
5. The language model is asked to redo arithmetic the code already does.

This design replaces them with one codebase built around a curated universe, a shared state store, a deterministic rules engine, a portfolio-level allocator, and a language-model reviewer that can only veto.

## 2. Goals and non-goals

**Goals**

- Sell weekly cash-secured puts only on names already approved for ownership, at prices already approved.
- Run the full wheel: puts, assignment, covered calls, call-away.
- Guarantee that every expiring short option has an explicit, recorded outcome.
- Fail closed: missing or stale data blocks a trade and never approves one.
- Record every decision with its inputs so results can be measured.

**Non-goals**

- Rolling. A roll stays a close plus an independent new entry.
- Discovering new companies automatically. The universe is curated by hand.
- Unattended order placement for opening trades in the first phases.
- Margin, naked calls, spreads, or earnings trades.

## 3. Design principles

| Principle | Consequence |
|---|---|
| Universe before premium | The scanner only sees approved symbols. |
| Code owns numbers | All arithmetic, thresholds and limits are deterministic and tested. |
| Model owns judgment, with veto only | The model can reject or flag. It cannot approve or change a number. |
| Intent is set by the human | Agents may propose a downgrade. They never upgrade or guess. |
| Fail closed | Unknown earnings date, missing score, stale quote: no trade. |
| Risk reduction is cheap, risk addition is gated | Closing orders face fewer gates than opening orders. |

## 4. Architecture

```
            +------------------+      +------------------+
            |  Universe file   |      |  Config (limits) |
            |  (human-edited)  |      |                  |
            +---------+--------+      +---------+--------+
                      |                         |
  +-----------+   +---v-------------------------v---+   +--------------+
  | Scanner   +--->                                 <---+ Broker       |
  | adapter   |   |          Rules engine           |   | adapter      |
  +-----------+   |  gates -> scores -> allocator   |   | (positions,  |
  +-----------+   |                                 |   |  quotes)     |
  | Events    +--->                                 |   +------^-------+
  | adapter   |   +---------------+-----------------+          |
  +-----------+                   |                            |
                         +--------v---------+                  |
                         |  Model reviewer  |                  |
                         |  (veto only)     |                  |
                         +--------+---------+                  |
                                  |                            |
                         +--------v---------+        +---------+------+
                         |  Approval gate   +-------->  Execution     |
                         |  (human)         |        |  (limit only)  |
                         +--------+---------+        +----------------+
                                  |
                         +--------v---------+
                         |  State store +   |
                         |  journal         |
                         +------------------+
```

### 4.1 Components

**Universe file.** A hand-edited list of 30–50 symbols. Each entry holds the thesis, the maximum price you would pay, the mode, and what would invalidate the thesis. Nothing outside this file can be traded.

**Scanner adapter.** Wraps Option Samurai (or a replacement). It passes the universe through the scan's `include_symbols` filter, fetches every page, and returns one normalized record per contract. It never ranks.

**Broker adapter.** Source of truth for positions, cash, cost basis, fills and live quotes. Read-only in phase 1.

**Events adapter.** Earnings dates, ex-dividend dates and headlines. Earnings dates are cross-checked against a second source, because a single wrong date defeats the main safety gate.

**Rules engine.** Pure functions with no I/O: gates, scores, allocation, exit rules and expiration rules. This is the only place a threshold lives.

**Model reviewer.** Reads the engine's shortlist plus recent headlines, and returns a structured verdict. See section 9.

**Approval gate.** Shows proposed orders and waits for a human yes.

**Execution.** Limit orders only, with idempotency keys and a kill switch.

**State store and journal.** One SQLite database. Agents read state from here and from the broker, never from each other's output.

### 4.2 Code layout

```
wheel-agent/
  src/
    types.ts            shared types (one definition of each)
    config.ts           limits and thresholds
    universe.ts         load and validate the universe file
    adapters/           scanner, broker, events
    engine/
      gates.ts          hard rejects
      scores.ts         ranking
      allocator.ts      portfolio-level sizing
      exits.ts          Tue–Thu rules
      expiration.ts     Friday rules
      calls.ts          covered-call and strangle rules
    reviewer.ts         model call and schema validation
    store.ts            state and journal
    runs/
      monday.ts  daily.ts  friday.ts
  test/
```

## 5. Data model

### 5.1 Universe entry

```json
{
  "symbol": "TGT",
  "sector": "Consumer Defensive",
  "thesis": "…",
  "invalidators": ["dividend cut", "guidance cut > 10%"],
  "maxBuyPrice": 112.00,
  "mode": "HOLD",
  "assignmentIntent": "allow",
  "callAwayIntent": "allow_above_cost_basis",
  "reviewedAt": "2026-11-02"
}
```

An entry whose `reviewedAt` is older than the configured age (*proposed* 90 days) is treated as blocked until reviewed.

### 5.2 Modes and intent

Mode describes the stock. Intent describes what you will accept at expiration. They are separate fields, which fixes the prototype's use of mode as a stand-in for intent.

| Mode | Meaning | New puts | Calls |
|---|---|---|---|
| ACCUMULATION | Want more at lower prices | Yes, including while holding shares | OTM only |
| HOLD | Keep, do not add | Only when no shares are held | OTM only |
| TRIM | Willing to sell | No | Near the money allowed |
| EXIT | Want out | No | ATM allowed |
| UNKNOWN | No recorded intent | No | None; close expiring options |

Rules for changes:

- Only the human can move a mode upward (toward ACCUMULATION) or set intent to `allow`.
- An agent can propose a downgrade. It applies immediately only when a listed invalidator is confirmed; otherwise it waits for approval.
- A missing input never changes a mode.

### 5.3 Journal

One row per decision: timestamp, run, symbol, contract, action, every input the engine used, gate results, scores, reviewer verdict, approval, order, fill. This is what makes calibration in section 12 possible.

## 6. Weekly schedule

| Run | When | Job |
|---|---|---|
| Monday | After the open settles | Review existing positions first, then open covered calls on owned shares, then open new puts |
| Tue–Thu | Once daily | Take profits, act on invalidated theses, set the Friday plan |
| Friday | Midday, and again before the close | Close, or confirm an intended expiration outcome |

Monday refuses to open anything until the position review has completed in the same run.

## 7. Entry rules

### 7.1 Gates (any failure rejects)

| Gate | Rule |
|---|---|
| Universe | Symbol is in the universe, reviewed, mode permits a new put |
| Price | Effective assignment price (strike − credit) ≤ `maxBuyPrice` |
| Earnings | Next earnings date is known, confirmed by two sources, and falls after expiration plus one trading day. Unknown date = `NEEDS_RESEARCH` |
| Dividend | For calls: no ex-dividend date before expiration when the call is near the money |
| Liquidity | Spread ≤ 15% of mid, open interest ≥ 100, volume ≥ 25, bid > 0 (*proposed*, carried from prototype) |
| Return | Credit meets the configured minimum. The prototype defined this but never checked it |
| Data freshness | Quote age under the configured limit |
| Completeness | Every field a gate or score uses is present. No favorable defaults |

### 7.2 Scores (rank the survivors)

The prototype's event-safety component was constant for every survivor, and its wheel-after-assignment component rewarded high covered-call yield, which is premium chasing by another name. Both are removed.

| Component | Weight (*proposed*) | Basis |
|---|---|---|
| Discount to max buy price | 35% | How far the effective assignment price sits below `maxBuyPrice` |
| Quality | 30% | Scanner scores plus margins, debt and liquidity ratios |
| Buffer | 20% | Distance to strike in units of expected weekly move |
| Premium | 15% | Capped; above the cap the score falls and a warning is raised |

### 7.3 Allocator (portfolio level)

Candidates are approved one at a time, best score first. Each approval reduces what remains for the next.

1. **Collateral.** Cash available = cash − collateral for open puts − reserve.
2. **Single name.** Shares held + all short put notional + the new put ≤ cap (*proposed* 10%).
3. **Lot size.** A 100-share lot must fit under the cap. In ACCUMULATION mode, two lots must fit. At $100k and a 10% cap that means strikes under $100, or under $50 for accumulation.
4. **Sector.** Same calculation at sector level (*proposed* 30%).
5. **Total short-put notional.** A cap on the whole book (*proposed* 50% of portfolio).
6. **Stress test.** Estimated loss if every position gaps down by a set amount (*proposed* 10%) must stay under a set share of the portfolio.
7. **Volatility regime.** Above a set volatility level, new notional is cut by a set fraction. Both values are open questions.

A candidate that fails any step is skipped, and the allocator moves to the next.

## 8. Position management

### 8.1 Covered calls and strangles

The prototype had no code for this half of the wheel.

- **HOLD / ACCUMULATION:** sell an OTM call only at a strike at or above cost basis.
- **TRIM / EXIT:** near-the-money or ATM calls are allowed. Selling below cost basis is permitted only in EXIT and is recorded as an accepted loss.
- **Covered strangle:** allowed only in ACCUMULATION, only when the allocator's two-lot check passes, and with the put strike at or below `maxBuyPrice`. The same-strike straddle is not implemented.

### 8.2 Tue–Thu exits

Rules are evaluated by priority. A lower-priority rule can never weaken a higher one, which fixes the prototype bug where negative news downgraded a close to an escalation.

| Priority | Condition | Action |
|---|---|---|
| 1 | A listed invalidator is confirmed | Close; propose EXIT |
| 2 | Assignment or call-away is no longer permitted by intent or limits | Close |
| 3 | Price-based stop reached (only for names where assignment intent is not `allow`) | Close |
| 4 | Premium captured ≥ 50% | Close for profit |
| 5 | Option is challenged, or headlines are material but ambiguous | Escalate to human |
| 6 | Otherwise | Hold, with the Friday plan recorded |

On stops: the prototype closed any put at 3x credit, which contradicts the premise that assignment is acceptable. Here, a name with `assignmentIntent: allow` is stopped by its thesis, not by the option price. A name without that intent should not have a put open at all, so the price stop is a backstop.

Buyback cost is always measured at the ask, not the mid.

### 8.3 Friday expiration

Valid outcomes: close, expire worthless, accept assignment, allow call-away. Default is close.

- **Safe to expire worthless:** distance to strike is at least a set multiple of the expected move for the time remaining (*proposed* 2x), and the option's ask is negligible. This replaces the fixed 2% rule, which ignored both volatility and time of day.
- **Accept assignment:** `assignmentIntent` is `allow`, thesis intact, allocator limits pass, and the effective price is still ≤ `maxBuyPrice`.
- **Allow call-away:** `callAwayIntent` permits it at this strike.
- **Late moves:** option holders can exercise after the closing bell, so an option that finishes slightly out of the money can still be assigned. Anything not clearly safe is closed in the pre-close run.

The run ends by asserting that every expiring short option has a recorded outcome. If the assertion fails, it alerts.

## 9. Model reviewer

**Input:** the engine's shortlist with all computed fields, the universe entry, and headlines from the last several days.

**Output (schema-validated):**

```json
{
  "symbol": "TGT",
  "verdict": "PASS | VETO | NEEDS_RESEARCH",
  "invalidatorHit": null,
  "reasons": ["…"],
  "sources": ["…"]
}
```

**Constraints:**

- It cannot approve anything the engine rejected, and it cannot alter a number.
- A malformed or missing response counts as `NEEDS_RESEARCH`.
- Headline text is untrusted input; instructions inside it are ignored.
- It also writes the human-readable memo, using only engine-computed figures.

## 10. Failure handling

| Failure | Behavior |
|---|---|
| Scanner or token unavailable | No new entries; management runs continue from broker data |
| Broker unavailable | Run aborts and alerts; nothing is assumed about positions |
| Stale quote | Position is escalated, not auto-held |
| Earnings sources disagree | `NEEDS_RESEARCH` |
| Duplicate run | Idempotency key prevents a second order |
| Kill switch set | All opening orders blocked; closing orders still allowed |

Dates use the exchange time zone throughout. The prototype derived "today" in UTC.

## 11. Prototype defects this design removes

| Defect | Fix |
|---|---|
| Missing earnings date treated as safe | Completeness and earnings gates |
| A past earnings date triggers a reject | Use next earnings date, validated as ≥ today |
| `??` fallbacks never fire on `NaN`; missing ratios default to favorable values | Explicit presence checks; no defaults |
| Only page 0 of a premium-sorted scan | Full pagination inside the universe |
| Candidates approved independently | Sequential allocator |
| `minReturnAcquisition`, `minAnnualizedReturn`, `NEEDS_RESEARCH` unused | Wired into gates and verdicts |
| Daily mode rewrite from default scores | Human-owned mode; downgrade-only proposals |
| News flag overrides a close | Priority-ordered exit rules |
| Friday uses mid, fixed 2%, and lets HOLD imply assignment | Ask pricing, expected-move test, explicit intent |
| Three copies of the types | One `types.ts` |
| Session token pasted into prompts and logs | Secrets from environment only; never logged |

## 12. Testing and rollout

**Tests**

- Unit tests for every gate, score and exit rule, including a missing-field case for each input.
- Replay tests: recorded scanner and broker snapshots run through the engine with expected decisions.
- Property test: no input combination with a missing required field yields an approval.

**Phases**

| Phase | What runs | Exit criterion |
|---|---|---|
| 0 | Consolidated code and tests | All tests pass on replayed data |
| 1 | Shadow mode: reports only, no orders | Four or more weeks with no decision you disagree with on review |
| 2 | Human-approved orders, one contract per name | Fills and journal match broker records |
| 3 | Closing orders automated; opening orders still approved | Your call |

**Measures**

- Realized share of puts finishing out of the money, against the scanner's stated probability.
- Assignment rate and the subsequent result of assigned shares.
- Fill price against mid.
- Return and drawdown against a simple benchmark, such as holding the index.

## 13. Open questions

1. **Data source.** The Option Samurai calls use a browser session token that expires in 24 hours. Is there a supported API key, or should the broker's option chain be the source?
2. **Broker.** Which one, and does its API expose cost basis and assignment notices?
3. **Limits.** Portfolio size, single-name cap, total notional cap, stress-test size and volatility threshold.
4. **Universe.** Who reviews it, and how often?
5. **Profit-taking.** Closing at 50% with midweek entries disabled leaves cash idle. Keep it idle, or allow re-entry through the full Monday process?
6. **Taxes.** Should the engine track wash sales and holding periods, or leave that to the broker?