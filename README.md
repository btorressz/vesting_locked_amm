# vesting_locked_amm

### 🔒 Vesting Locked AMM

A sophisticated Solana-based Automated Market Maker (AMM) that combines traditional liquidity provision with a unique **vesting mechanism**. This protocol incentivizes long-term liquidity commitment by locking LP tokens in vesting contracts while distributing rewards over time.

---


### 🌟 Overview

The **Vesting Locked AMM** addresses liquidity instability in DeFi protocols. Rather than allowing instant LP withdrawals, it enforces **vesting periods (30–180 days)** to create durable, stable liquidity pools and reward committed liquidity providers.

---

### ✨ Key Features

#### 🏊‍♂️ Liquidity Pool Management

- **Constant Product Formula:** Uses the classic `x * y = k` model.
- **Dual Token Pools:** Supports two tokens per pool (Token A and Token B).
- **LP Token Minting:** Issues LP tokens for proportional pool ownership.

#### ⏰ Vesting System

- **Time-Locked Deposits:** LP tokens are locked for 30–180 days.
- **Gradual Release:** Withdrawals only allowed post-vesting.
- **Early Exit Penalties:** Premature exits incur a penalty sent to the treasury.

