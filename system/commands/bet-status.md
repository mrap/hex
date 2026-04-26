---
name: bet-status
description: >
  Live prediction market bet status. Polls current prices from Kalshi, reads
  arena leaderboard, and calculates P&L for active positions.
---

# /bet-status — Live Prediction Market Status

Show current status of all active prediction market positions with live prices,
arena leaderboard context, and P&L calculation.

## Step 1: Read active positions

Read `projects/prediction-market/checkpoint.md` and
`projects/prediction-market/bets/opus-4-7-thinking-rank-1-by-2026-04-30.md`
to get the current position details.

Known positions (use these as authoritative entry data):

| Leg | Ticker | Side | Entry price | Position $ |
|---|---|---|---|---|
| A | `KXAIMODEL-T4` (`claude-opus-4-6-thinking`) | NO | 0.31 | $3,020.93 |
| B | `KXAIMODEL-T5` (`claude-opus-4-7-thinking`) | YES | 0.06 | $1,164.90 |

**Settlement:** 2026-04-30 (Arena `text/overall` leaderboard)

## Step 2: Fetch live market prices

Run market-price-poll.sh for each position. Use Bash to capture results:

```bash
cd /Users/mrap/mrap-hex
bash .hex/bin/market-price-poll.sh "KXAIMODEL-T4" 2>/dev/null
bash .hex/bin/market-price-poll.sh "KXAIMODEL-T5" 2>/dev/null
```

If either call returns an error or null prices, note it as "API unavailable" and
skip P&L for that leg rather than crashing.

## Step 3: Read latest arena leaderboard

Find the most recent `text-overall-*.md` file in
`projects/arena-leaderboard/history/` and read it. Extract:
- Rank and rating of `claude-opus-4-7-thinking`
- Rank and rating of `claude-opus-4-6-thinking` (the #1 incumbent)
- Gap between them
- Days remaining until 2026-04-30

## Step 4: Calculate P&L

For each leg, calculate using:

```
shares        = position_$ / entry_price
current_value = shares × current_price
unrealized_pl = current_value - position_$
pl_pct        = unrealized_pl / position_$  × 100
```

For Leg A (NO position):
- `current_price` = `no_price` from API (i.e., `1 - yes_price`)

For Leg B (YES position):
- `current_price` = `yes_price` from API

## Step 5: Assess position vs decision framework

Apply the decision framework from the bet file:

**Leg A (NO on 4-6-thinking):**
- YES price < 20%  → Hold (underpriced vs fair)
- YES price 20–25% → Hold (fair)
- YES price > 25%  → **Cash out** (overpriced; time decay against)

**Leg B (YES on 4-7-thinking):**
- YES price < 5%   → Add (underpriced edge)
- YES price 5–10%  → Hold (fair)
- YES price > 10%  → Trim

## Step 6: Output status

Produce a concise, scannable status block:

```
━━━ BET STATUS — [YYYY-MM-DD HH:MM ET] ━━━

ARENA (text/overall)
  claude-opus-4-7-thinking  #[rank]  rating [N]  gap [±N] to #1
  claude-opus-4-6-thinking  #[rank]  rating [N]  (incumbent)
  Days to settlement: [N]

MARKETS
  Leg A  KXAIMODEL-T4  NO @ 31% entry
    Current YES: [N]%   NO: [N]%
    P&L: [+/-$N] ([+/-N]%)  →  [action signal]

  Leg B  KXAIMODEL-T5  YES @ 6% entry
    Current YES: [N]%   NO: [N]%
    P&L: [+/-$N] ([+/-N]%)  →  [action signal]

TOTAL
  Entry capital:  ~$5,550
  Current value:  $[N]
  Unrealized P&L: [+/-$N] ([+/-N]%)

ASSESSMENT  [one sentence on whether to hold/exit/add]
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
```

Keep the output tight — no preamble, no markdown headers outside the block.
