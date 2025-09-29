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

  #### 💰 Fee Distribution

- **Protocol Fees:** Charged on every token swap.
- **Treasury Split:** A portion goes to the protocol treasury.
- **Reward Distribution:** The rest is distributed to vesting participants.
- **Auto-Compounding:** Rewards grow based on vesting duration and size.

#### 🎯 Reward Mechanism

- **Proportional Rewards:** Based on amount & duration of vesting.
- **Scaled Accounting:** Uses `acc_reward_per_lp` for high-precision tracking.
- **Debt Tracking:** Prevents double-claiming using `reward_debt`.

---

### 🏗️ Core Functions

#### 🔧 Pool Management

- `initialize_pool`: Sets up pool and configures fees.
- `pause / unpause`: Emergency trading halt switches.
- `emergency_withdraw`: Authority drains reserves during crises.

#### 💼 Liquidity Operations

- `deposit_and_vest`: Users deposit tokens & lock LP tokens.
- `claim_vested`: Withdraws LP + rewards after vesting ends.
- `early_unvest`: Early withdrawal with treasury penalty.
- `withdraw_unlocked`: Burns LP tokens to return Token A & B.

#### 🔁 Trading

- `swap`: Performs token swaps using `x*y=k` formula with fees.

---

### 📊 Core Data Structures

#### 📦 Pool Account

Contains:

- `authority`: Admin with emergency controls
- `token_a_mint`, `token_b_mint`
- `lp_mint`: LP token mint
- `reserve_a`, `reserve_b`: Reserve accounts
- `protocol_fee_bps`, `treasury_fee_bps`, `reward_fee_bps`
- `vesting_nonce`: Vesting ID counter
- `paused`: Trading status
- `acc_reward_per_lp`: Global rewards tracker

#### 📄 VestingStake Account

Tracks each user's vesting:

- `pool`, `user`
- `amount`: Locked LP tokens
- `vesting_end`: Vesting end timestamp
- `claimed`: Boolean
- `deposit_id`: Unique ID
- `reward_debt`: Reward baseline

---

### 🧾 Events

- `PoolInitialized`
- `Deposited`
- `Claimed`
- `EarlyUnvested`
- `Withdrawn`
- `Swapped`
- `Paused / Unpaused`
- `EmergencyWithdrawn`

---

### ⚠️ Error Handling

- `InvalidVestingPeriod`
- `NumericOverflow`
- `InsufficientLiquidity`
- `VestingNotFinished`
- `AlreadyClaimed`
- `SlippageExceeded`
- `Paused`
- `InvalidFeeSplit`

---

### 🔧 Technical Implementation

#### 🔐 Security

- **PDA Authority** for pool control
- **Rent Checks** for all token accounts
- **Ownership Validation** for SPL accounts
- **Overflow Protection** throughout

#### 💸 Fee Mechanism

- Collected on swaps
- Split into treasury + rewards
- Residual stays in reserves

#### 🎁 Reward Accounting

- **Global:** `acc_reward_per_lp`
- **User:** `reward_debt`
- **Pending:** `rewards = (amount * acc) - debt`

---


