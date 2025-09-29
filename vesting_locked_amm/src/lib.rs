use anchor_lang::prelude::*;
use anchor_spl::token::{self, Mint, Token, TokenAccount, Transfer, MintTo, Burn, SetAuthority};
use spl_token::instruction::AuthorityType as SplAuthorityType;

declare_id!("sbH7oanT87wMjAxwv6GHsBFiDAHA6GvHF8TWxALRiQS");

const REWARD_SCALE: u128 = 1_000_000_000_000u128; // scaling for acc rewards to keep precision

#[program]
pub mod vesting_locked_amm {
    use super::*;

    /// Initialize pool and transfer LP-mint authority to the pool PDA.
    /// Also configures treasury split and reward fee split.
    pub fn initialize_pool(
        ctx: Context<InitializePool>,
        protocol_fee_bps: u16,
        treasury_fee_bps: u16,
        reward_fee_bps: u16,
    ) -> Result<()> {
        // basic fee split sanity check
        require!(
            treasury_fee_bps
                .checked_add(reward_fee_bps)
                .unwrap_or(u16::MAX)
                <= protocol_fee_bps,
            AmmError::InvalidFeeSplit
        );

        let pool = &mut ctx.accounts.pool;
        pool.authority = *ctx.accounts.authority.key;
        pool.token_a_mint = ctx.accounts.token_a_mint.key();
        pool.token_b_mint = ctx.accounts.token_b_mint.key();
        pool.lp_mint = ctx.accounts.lp_mint.key();
        pool.reserve_a = ctx.accounts.reserve_a.key();
        pool.reserve_b = ctx.accounts.reserve_b.key();
        pool.protocol_fee_bps = protocol_fee_bps;
        pool.treasury = ctx.accounts.treasury.key();
        pool.treasury_fee_bps = treasury_fee_bps;
        pool.reward_fee_bps = reward_fee_bps;
        pool.vesting_nonce = 0;
        pool.paused = false;
        pool.acc_reward_per_lp = 0u128;

        // Transfer LP mint authority to the pool PDA.
        // The current authority (ctx.accounts.authority) must be the current mint authority and sign this tx.
        let pool_key = pool.key();
        let cpi_accounts = SetAuthority {
            account_or_mint: ctx.accounts.lp_mint.to_account_info().clone(),
            current_authority: ctx.accounts.authority.to_account_info().clone(),
        };
        token::set_authority(
            CpiContext::new(ctx.accounts.token_program.to_account_info().clone(), cpi_accounts),
            SplAuthorityType::MintTokens,
            Some(pool_key),
        )?;

        emit!(PoolInitialized {
            pool: pool.key(),
            authority: pool.authority,
            treasury: pool.treasury,
        });

        Ok(())
    }

    /// Deposit tokens A+B and mint LP tokens, but lock them into a vesting PDA until `vesting_seconds` passes.
    /// This instruction program-creates the vesting token account (owned by the vesting PDA) to simplify client UX.
    pub fn deposit_and_vest(
        ctx: Context<DepositAndVest>,
        amount_a: u64,
        amount_b: u64,
        vesting_seconds: i64,
    ) -> Result<()> {
        // Read immutable bits first (avoid mutable borrow while building CPI contexts)
        require!(!ctx.accounts.pool.paused, AmmError::Paused);

        // Enforce vesting window
        let min_vesting = 30 * 24 * 3600;
        let max_vesting = 180 * 24 * 3600;
        require!(
            vesting_seconds >= min_vesting && vesting_seconds <= max_vesting,
            AmmError::InvalidVestingPeriod
        );

        // Defensive checks: require reserve token accounts to be rent-exempt and owned by token program
        let rent = Rent::get()?;
        require!(
            rent.is_exempt(
                ctx.accounts.reserve_a.to_account_info().lamports(),
                ctx.accounts.reserve_a.to_account_info().data_len()
            ),
            AmmError::NotRentExempt
        );
        require!(
            rent.is_exempt(
                ctx.accounts.reserve_b.to_account_info().lamports(),
                ctx.accounts.reserve_b.to_account_info().data_len()
            ),
            AmmError::NotRentExempt
        );
        require!(
            ctx.accounts.reserve_a.to_account_info().owner == &token::ID,
            AmmError::InvalidTokenAccountOwner
        );
        require!(
            ctx.accounts.reserve_b.to_account_info().owner == &token::ID,
            AmmError::InvalidTokenAccountOwner
        );

        // Capture some values we will need after CPIs
        let pool_key = ctx.accounts.pool.key();
        // vesting_stake PDA was created with seeds involving current pool.vesting_nonce; Anchor validated that already.
        let current_vesting_nonce = ctx.accounts.pool.vesting_nonce;

        // Transfer token A and B from user to pool reserves (CPIs)
        token::transfer(ctx.accounts.transfer_a_context(), amount_a)?;
        token::transfer(ctx.accounts.transfer_b_context(), amount_b)?;

        // Calculate LP amount to mint using post-transfer reserve amounts (reading token accounts directly)
        let lp_minted = calculate_lp_mint_amount(
            amount_a,
            amount_b,
            ctx.accounts.reserve_a.amount,
            ctx.accounts.reserve_b.amount,
            ctx.accounts.lp_mint.supply,
        )?;

        // Mint LP tokens to the vesting token account (owned by vesting PDA)
        token::mint_to(ctx.accounts.mint_to_vesting_context(), lp_minted)?;

        // Now mutate pool & vesting accounts (safe: no active CPI borrows)
        let pool = &mut ctx.accounts.pool;
        let vesting = &mut ctx.accounts.vesting_stake;

        vesting.pool = pool_key;
        vesting.user = ctx.accounts.user.key();
        vesting.amount = lp_minted;
        let clock = Clock::get()?;
        vesting.vesting_end = clock.unix_timestamp + vesting_seconds;
        vesting.claimed = false;
        vesting.deposit_id = current_vesting_nonce;

        // Reward accounting snapshot
        vesting.reward_debt = (u128::from(lp_minted) * pool.acc_reward_per_lp) / REWARD_SCALE;

        pool.vesting_nonce = pool
            .vesting_nonce
            .checked_add(1)
            .ok_or(AmmError::NumericOverflow)?;

        emit!(Deposited {
            pool: pool_key,
            user: vesting.user,
            amount: vesting.amount,
            vesting_end: vesting.vesting_end,
        });

        Ok(())
    }

    /// Claim the vested LP tokens (transfer them from the vesting token account to the user's LP token account)
    pub fn claim_vested(ctx: Context<ClaimVested>) -> Result<()> {
        // Read required values immutably
        require!(!ctx.accounts.pool.paused, AmmError::Paused);
        let vesting_amount = ctx.accounts.vesting_stake.amount;
        let vesting_end = ctx.accounts.vesting_stake.vesting_end;
        let vesting_claimed = ctx.accounts.vesting_stake.claimed;
        let vesting_reward_debt = ctx.accounts.vesting_stake.reward_debt;

        require!(!vesting_claimed, AmmError::AlreadyClaimed);
        let clock = Clock::get()?;
        require!(clock.unix_timestamp >= vesting_end, AmmError::VestingNotFinished);

        // Compute pending reward (in LP-equivalent units using acc_reward_per_lp snapshot)
        let total_reward_for_stake = (u128::from(vesting_amount) * ctx.accounts.pool.acc_reward_per_lp) / REWARD_SCALE;
        let pending_reward = total_reward_for_stake.checked_sub(vesting_reward_debt).unwrap_or(0u128);

        // Perform transfers (CPIs) while only immutable borrows in scope
        token::transfer(ctx.accounts.transfer_from_vesting_context(), vesting_amount)?;

        if pending_reward > 0 {
            let pending_u64: u64 = pending_reward.try_into().map_err(|_| AmmError::NumericOverflow)?;
            if ctx.accounts.reward_vault.amount >= pending_u64 {
                token::transfer(ctx.accounts.transfer_reward_to_user_context(), pending_u64)?;
            }
        }

        // Now mutate vesting account (safe)
        let vesting = &mut ctx.accounts.vesting_stake;
        vesting.claimed = true;

        emit!(Claimed {
            pool: ctx.accounts.pool.key(),
            user: vesting.user,
            amount: vesting.amount,
        });

        Ok(())
    }

    /// Allow early unvest (partial or full) with penalty. Penalty is sent to treasury LP token account.
    pub fn early_unvest(
        ctx: Context<EarlyUnvest>,
        lp_amount: u64,
        penalty_bps: u16,
    ) -> Result<()> {
        require!(!ctx.accounts.pool.paused, AmmError::Paused);
        require!(penalty_bps <= 10_000, AmmError::InvalidPenalty);

        // Read vesting immutable fields first
        let vesting_amount = ctx.accounts.vesting_stake.amount;
        let vesting_claimed = ctx.accounts.vesting_stake.claimed;
        require!(!vesting_claimed, AmmError::AlreadyClaimed);
        require!(lp_amount <= vesting_amount, AmmError::InsufficientVestedAmount);

        let penalty_lp = (u128::from(lp_amount) * u128::from(penalty_bps) / 10_000u128) as u64;
        let amount_to_user = lp_amount.checked_sub(penalty_lp).ok_or(AmmError::NumericOverflow)?;

        // Transfers: penalty -> treasury, remainder -> user
        if penalty_lp > 0 {
            token::transfer(ctx.accounts.transfer_penalty_to_treasury_context(), penalty_lp)?;
        }
        if amount_to_user > 0 {
            token::transfer(ctx.accounts.transfer_from_vesting_context(), amount_to_user)?;
        }

        // Update vesting account
        let vesting = &mut ctx.accounts.vesting_stake;
        vesting.amount = vesting.amount.checked_sub(lp_amount).ok_or(AmmError::NumericOverflow)?;
        if vesting.amount == 0 {
            vesting.claimed = true;
        }

        emit!(EarlyUnvested {
            pool: ctx.accounts.pool.key(),
            user: vesting.user,
            amount_unvested: lp_amount,
            penalty: penalty_lp,
        });

        Ok(())
    }

    /// Burn unlocked LP tokens and withdraw proportional amounts of token A and B from pool reserves.
    pub fn withdraw_unlocked(ctx: Context<Withdraw>, lp_amount: u64) -> Result<()> {
        require!(!ctx.accounts.pool.paused, AmmError::Paused);

        let lp_supply = ctx.accounts.lp_mint.supply;
        require!(lp_supply > 0, AmmError::InsufficientLiquidity);

        let amount_a = (u128::from(ctx.accounts.reserve_a.amount)
            .checked_mul(u128::from(lp_amount))
            .ok_or(AmmError::NumericOverflow)?
            / u128::from(lp_supply)) as u64;

        let amount_b = (u128::from(ctx.accounts.reserve_b.amount)
            .checked_mul(u128::from(lp_amount))
            .ok_or(AmmError::NumericOverflow)?
            / u128::from(lp_supply)) as u64;

        token::burn(ctx.accounts.burn_lp_context(), lp_amount)?;
        token::transfer(ctx.accounts.transfer_a_to_user_context(), amount_a)?;
        token::transfer(ctx.accounts.transfer_b_to_user_context(), amount_b)?;

        emit!(Withdrawn {
            pool: ctx.accounts.pool.key(),
            user: ctx.accounts.user.key(),
            lp_amount,
            amount_a,
            amount_b,
        });

        Ok(())
    }

    /// Simple constant-product swap with protocol fee charged (fee goes to the pool reserves).
    /// A portion of the protocol fee is routed to treasury and a portion to the reward pool (simple model).
    pub fn swap(
        ctx: Context<Swap>,
        amount_in: u64,
        minimum_amount_out: u64,
        is_a_to_b: bool,
        min_slot: Option<u64>,
    ) -> Result<()> {
        require!(!ctx.accounts.pool.paused, AmmError::Paused);

        if let Some(ms) = min_slot {
            let clock = Clock::get()?;
            require!(clock.slot >= ms, AmmError::SlotTooLow);
        }

        // Read values immutably
        let fee_bps = u128::from(ctx.accounts.pool.protocol_fee_bps);
        let fee_denom = 10_000u128;

        let (reserve_in_amount, reserve_out_amount) = if is_a_to_b {
            (u128::from(ctx.accounts.reserve_a.amount), u128::from(ctx.accounts.reserve_b.amount))
        } else {
            (u128::from(ctx.accounts.reserve_b.amount), u128::from(ctx.accounts.reserve_a.amount))
        };

        require!(
            reserve_in_amount > 0 && reserve_out_amount > 0,
            AmmError::InsufficientLiquidity
        );

        let amount_in_u128 = u128::from(amount_in);
        let amount_in_after_fee = amount_in_u128
            .checked_mul(fee_denom.checked_sub(fee_bps).ok_or(AmmError::NumericOverflow)?)
            .ok_or(AmmError::NumericOverflow)?
            / fee_denom;

        let total_fee = amount_in_u128.checked_sub(amount_in_after_fee).ok_or(AmmError::NumericOverflow)?;

        let treasury_fee = (total_fee * u128::from(ctx.accounts.pool.treasury_fee_bps))
            / u128::from(ctx.accounts.pool.protocol_fee_bps.max(1));
        let reward_fee = (total_fee * u128::from(ctx.accounts.pool.reward_fee_bps))
            / u128::from(ctx.accounts.pool.protocol_fee_bps.max(1));
        let _to_reserve_fee = total_fee
            .checked_sub(treasury_fee)
            .ok_or(AmmError::NumericOverflow)?
            .checked_sub(reward_fee)
            .ok_or(AmmError::NumericOverflow)?;

        // Compute new acc_reward_per_lp locally (no mutable borrow)
        let total_locked_lp = ctx.accounts.lp_mint.supply; // naive
        let mut acc_reward_per_lp_local = ctx.accounts.pool.acc_reward_per_lp;
        if total_locked_lp > 0 && reward_fee > 0 {
            acc_reward_per_lp_local = acc_reward_per_lp_local
                .checked_add((reward_fee * REWARD_SCALE) / u128::from(total_locked_lp))
                .ok_or(AmmError::NumericOverflow)?;
        }

        // constant-product calc
        let k = reserve_in_amount.checked_mul(reserve_out_amount).ok_or(AmmError::NumericOverflow)?;
        let new_reserve_in = reserve_in_amount.checked_add(amount_in_after_fee).ok_or(AmmError::NumericOverflow)?;
        let new_reserve_out = k.checked_div(new_reserve_in).ok_or(AmmError::NumericOverflow)?;
        let amount_out_u128 = reserve_out_amount.checked_sub(new_reserve_out).ok_or(AmmError::NumericOverflow)?;
        let amount_out = amount_out_u128 as u64;
        require!(amount_out >= minimum_amount_out, AmmError::SlippageExceeded);

        // Do CPIs (transfers)
        if is_a_to_b {
            token::transfer(ctx.accounts.transfer_in_a_context(), amount_in)?;
            token::transfer(ctx.accounts.transfer_out_b_context(), amount_out)?;
            if treasury_fee > 0 {
                let t_fee: u64 = treasury_fee.try_into().map_err(|_| AmmError::NumericOverflow)?;
                token::transfer(ctx.accounts.transfer_treasury_from_reserve_a_context(), t_fee)?;
            }
        } else {
            token::transfer(ctx.accounts.transfer_in_b_context(), amount_in)?;
            token::transfer(ctx.accounts.transfer_out_a_context(), amount_out)?;
            if treasury_fee > 0 {
                let t_fee: u64 = treasury_fee.try_into().map_err(|_| AmmError::NumericOverflow)?;
                token::transfer(ctx.accounts.transfer_treasury_from_reserve_b_context(), t_fee)?;
            }
        }

        // Now mutate pool.acc_reward_per_lp
        let pool = &mut ctx.accounts.pool;
        pool.acc_reward_per_lp = acc_reward_per_lp_local;

        emit!(Swapped {
            pool: ctx.accounts.pool.key(),
            user: ctx.accounts.user.key(),
            amount_in,
            amount_out,
            is_a_to_b,
        });

        Ok(())
    }

    pub fn pause(ctx: Context<OnlyAuthority>) -> Result<()> {
        let pool = &mut ctx.accounts.pool;
        pool.paused = true;
        emit!(Paused { pool: pool.key() });
        Ok(())
    }

    pub fn unpause(ctx: Context<OnlyAuthority>) -> Result<()> {
        let pool = &mut ctx.accounts.pool;
        pool.paused = false;
        emit!(Unpaused { pool: pool.key() });
        Ok(())
    }

    pub fn emergency_withdraw(ctx: Context<EmergencyWithdraw>) -> Result<()> {
        // Transfers while only immutable reads used earlier
        let reserve_a_bal = ctx.accounts.reserve_a.amount;
        let reserve_b_bal = ctx.accounts.reserve_b.amount;
        if reserve_a_bal > 0 {
            token::transfer(ctx.accounts.transfer_reserve_a_to_treasury_context(), reserve_a_bal)?;
        }
        if reserve_b_bal > 0 {
            token::transfer(ctx.accounts.transfer_reserve_b_to_treasury_context(), reserve_b_bal)?;
        }
        emit!(EmergencyWithdrawn { pool: ctx.accounts.pool.key() });
        Ok(())
    }
}

// ---------------------- Accounts ----------------------

#[account]
pub struct Pool {
    pub authority: Pubkey,
    pub token_a_mint: Pubkey,
    pub token_b_mint: Pubkey,
    pub lp_mint: Pubkey,
    pub reserve_a: Pubkey,
    pub reserve_b: Pubkey,
    pub protocol_fee_bps: u16,
    pub treasury: Pubkey,
    pub treasury_fee_bps: u16,
    pub reward_fee_bps: u16,
    pub vesting_nonce: u64,
    pub paused: bool,
    pub acc_reward_per_lp: u128, // scaled by REWARD_SCALE
}

#[account]
pub struct VestingStake {
    pub pool: Pubkey,
    pub user: Pubkey,
    pub amount: u64,
    pub vesting_end: i64,
    pub claimed: bool,
    pub deposit_id: u64,
    pub reward_debt: u128,
}

// ---------------------- Events ----------------------

#[event]
pub struct PoolInitialized {
    pub pool: Pubkey,
    pub authority: Pubkey,
    pub treasury: Pubkey,
}
#[event]
pub struct Deposited {
    pub pool: Pubkey,
    pub user: Pubkey,
    pub amount: u64,
    pub vesting_end: i64,
}
#[event]
pub struct Claimed {
    pub pool: Pubkey,
    pub user: Pubkey,
    pub amount: u64,
}
#[event]
pub struct EarlyUnvested {
    pub pool: Pubkey,
    pub user: Pubkey,
    pub amount_unvested: u64,
    pub penalty: u64,
}
#[event]
pub struct Withdrawn {
    pub pool: Pubkey,
    pub user: Pubkey,
    pub lp_amount: u64,
    pub amount_a: u64,
    pub amount_b: u64,
}
#[event]
pub struct Swapped {
    pub pool: Pubkey,
    pub user: Pubkey,
    pub amount_in: u64,
    pub amount_out: u64,
    pub is_a_to_b: bool,
}
#[event]
pub struct Paused {
    pub pool: Pubkey,
}
#[event]
pub struct Unpaused {
    pub pool: Pubkey,
}
#[event]
pub struct EmergencyWithdrawn {
    pub pool: Pubkey,
}

// ---------------------- Contexts ----------------------

#[derive(Accounts)]
pub struct InitializePool<'info> {
    #[account(init, payer = authority, space = 8 + 256, seeds = [b"pool", lp_mint.key().as_ref()], bump)]
    pub pool: Account<'info, Pool>,
    #[account(mut)]
    pub authority: Signer<'info>,
    pub token_a_mint: Account<'info, Mint>,
    pub token_b_mint: Account<'info, Mint>,
    #[account(mut)]
    pub lp_mint: Account<'info, Mint>,
    /// CHECK: token accounts created by client
    #[account(mut)]
    pub reserve_a: AccountInfo<'info>,
    /// CHECK: token accounts created by client
    #[account(mut)]
    pub reserve_b: AccountInfo<'info>,
    /// CHECK: treasury token account (must be a token account for LP tokens for penalty/tax routing)
    #[account(mut)]
    pub treasury: AccountInfo<'info>,
    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
    pub rent: Sysvar<'info, Rent>,
}

#[derive(Accounts)]
#[instruction(amount_a: u64, amount_b: u64, vesting_seconds: i64)]
pub struct DepositAndVest<'info> {
    #[account(mut, has_one = lp_mint, seeds = [b"pool", lp_mint.key().as_ref()], bump)]
    pub pool: Account<'info, Pool>,
    #[account(mut)]
    pub lp_mint: Account<'info, Mint>,

    #[account(mut, token::mint = token_a_mint)]
    pub reserve_a: Account<'info, TokenAccount>,
    #[account(mut, token::mint = token_b_mint)]
    pub reserve_b: Account<'info, TokenAccount>,

    #[account(mut)]
    pub user: Signer<'info>,

    #[account(mut, token::mint = token_a_mint, token::authority = user)]
    pub user_token_a: Account<'info, TokenAccount>,
    #[account(mut, token::mint = token_b_mint, token::authority = user)]
    pub user_token_b: Account<'info, TokenAccount>,

    /// Vesting PDA (unique per deposit)
    #[account(
        init,
        payer = user,
        space = 8 + 128,
        seeds = [b"vesting", pool.key().as_ref(), user.key().as_ref(), &pool.vesting_nonce.to_le_bytes()],
        bump
    )]
    pub vesting_stake: Account<'info, VestingStake>,

    /// Vesting token account to hold LP tokens. Program creates it and sets authority to the vesting PDA.
    #[account(
        init,
        payer = user,
        token::mint = lp_mint,
        token::authority = vesting_stake,
        seeds = [b"vesting_vault", pool.key().as_ref(), user.key().as_ref(), &pool.vesting_nonce.to_le_bytes()],
        bump
    )]
    pub vesting_token_account: Account<'info, TokenAccount>,

    /// Reward vault (optional) where reward LP tokens are stored for distribution
    #[account(mut, token::mint = lp_mint)]
    pub reward_vault: Account<'info, TokenAccount>,

    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
    pub rent: Sysvar<'info, Rent>,
    pub token_a_mint: Account<'info, Mint>,
    pub token_b_mint: Account<'info, Mint>,
}

impl<'info> DepositAndVest<'info> {
    fn transfer_a_context(&self) -> CpiContext<'_, '_, '_, 'info, Transfer<'info>> {
        let cpi_accounts = Transfer {
            from: self.user_token_a.to_account_info().clone(),
            to: self.reserve_a.to_account_info().clone(),
            authority: self.user.to_account_info().clone(),
        };
        CpiContext::new(self.token_program.to_account_info().clone(), cpi_accounts)
    }
    fn transfer_b_context(&self) -> CpiContext<'_, '_, '_, 'info, Transfer<'info>> {
        let cpi_accounts = Transfer {
            from: self.user_token_b.to_account_info().clone(),
            to: self.reserve_b.to_account_info().clone(),
            authority: self.user.to_account_info().clone(),
        };
        CpiContext::new(self.token_program.to_account_info().clone(), cpi_accounts)
    }

    fn mint_to_vesting_context(&self) -> CpiContext<'_, '_, '_, 'info, MintTo<'info>> {
        let cpi_accounts = MintTo {
            mint: self.lp_mint.to_account_info().clone(),
            to: self.vesting_token_account.to_account_info().clone(),
            authority: self.pool.to_account_info().clone(), // pool PDA is mint authority
        };
        CpiContext::new(self.token_program.to_account_info().clone(), cpi_accounts)
    }
}

#[derive(Accounts)]
pub struct ClaimVested<'info> {
    #[account(mut, has_one = lp_mint, seeds = [b"pool", lp_mint.key().as_ref()], bump)]
    pub pool: Account<'info, Pool>,
    #[account(mut)]
    pub lp_mint: Account<'info, Mint>,

    #[account(mut, close = user)]
    pub vesting_stake: Account<'info, VestingStake>,

    /// Vesting token account owned by vesting PDA
    #[account(mut, token::authority = vesting_stake)]
    pub vesting_token_account: Account<'info, TokenAccount>,

    /// destination LP token account of the user
    #[account(mut, token::mint = lp_mint, token::authority = user)]
    pub user_lp_token_account: Account<'info, TokenAccount>,

    #[account(mut)]
    pub user: Signer<'info>,

    /// Reward vault where reward LPs are held
    #[account(mut, token::mint = lp_mint)]
    pub reward_vault: Account<'info, TokenAccount>,

    pub token_program: Program<'info, Token>,
}

impl<'info> ClaimVested<'info> {
    fn transfer_from_vesting_context(&self) -> CpiContext<'_, '_, '_, 'info, Transfer<'info>> {
        let cpi_accounts = Transfer {
            from: self.vesting_token_account.to_account_info().clone(),
            to: self.user_lp_token_account.to_account_info().clone(),
            authority: self.vesting_stake.to_account_info().clone(),
        };
        CpiContext::new(self.token_program.to_account_info().clone(), cpi_accounts)
    }
    fn transfer_reward_to_user_context(&self) -> CpiContext<'_, '_, '_, 'info, Transfer<'info>> {
        let cpi_accounts = Transfer {
            from: self.reward_vault.to_account_info().clone(),
            to: self.user_lp_token_account.to_account_info().clone(),
            authority: self.pool.to_account_info().clone(),
        };
        CpiContext::new(self.token_program.to_account_info().clone(), cpi_accounts)
    }
}

#[derive(Accounts)]
pub struct EarlyUnvest<'info> {
    #[account(mut, has_one = lp_mint, seeds = [b"pool", lp_mint.key().as_ref()], bump)]
    pub pool: Account<'info, Pool>,
    #[account(mut)]
    pub lp_mint: Account<'info, Mint>,

    #[account(mut)]
    pub vesting_stake: Account<'info, VestingStake>,

    /// Vesting token account owned by vesting PDA
    #[account(mut, token::authority = vesting_stake)]
    pub vesting_token_account: Account<'info, TokenAccount>,

    /// user's LP account
    #[account(mut, token::mint = lp_mint, token::authority = user)]
    pub user_lp_token_account: Account<'info, TokenAccount>,

    /// treasury LP token account to receive penalties
    #[account(mut, token::mint = lp_mint)]
    pub treasury_lp_account: Account<'info, TokenAccount>,

    #[account(mut)]
    pub user: Signer<'info>,

    pub token_program: Program<'info, Token>,
}

impl<'info> EarlyUnvest<'info> {
    fn transfer_penalty_to_treasury_context(&self) -> CpiContext<'_, '_, '_, 'info, Transfer<'info>> {
        let cpi_accounts = Transfer {
            from: self.vesting_token_account.to_account_info().clone(),
            to: self.treasury_lp_account.to_account_info().clone(),
            authority: self.vesting_stake.to_account_info().clone(),
        };
        CpiContext::new(self.token_program.to_account_info().clone(), cpi_accounts)
    }

    fn transfer_from_vesting_context(&self) -> CpiContext<'_, '_, '_, 'info, Transfer<'info>> {
        let cpi_accounts = Transfer {
            from: self.vesting_token_account.to_account_info().clone(),
            to: self.user_lp_token_account.to_account_info().clone(),
            authority: self.vesting_stake.to_account_info().clone(),
        };
        CpiContext::new(self.token_program.to_account_info().clone(), cpi_accounts)
    }
}

#[derive(Accounts)]
pub struct Withdraw<'info> {
    #[account(mut, has_one = lp_mint, seeds = [b"pool", lp_mint.key().as_ref()], bump)]
    pub pool: Account<'info, Pool>,
    #[account(mut)]
    pub lp_mint: Account<'info, Mint>,
    #[account(mut)]
    pub reserve_a: Account<'info, TokenAccount>,
    #[account(mut)]
    pub reserve_b: Account<'info, TokenAccount>,

    #[account(mut)]
    pub user: Signer<'info>,
    #[account(mut, token::mint = lp_mint, token::authority = user)]
    pub user_lp_token_account: Account<'info, TokenAccount>,
    #[account(mut, token::mint = token_a_mint, token::authority = user)]
    pub user_token_a: Account<'info, TokenAccount>,
    #[account(mut, token::mint = token_b_mint, token::authority = user)]
    pub user_token_b: Account<'info, TokenAccount>,

    pub token_program: Program<'info, Token>,
    pub token_a_mint: Account<'info, Mint>,
    pub token_b_mint: Account<'info, Mint>,
}

impl<'info> Withdraw<'info> {
    fn burn_lp_context(&self) -> CpiContext<'_, '_, '_, 'info, Burn<'info>> {
        let cpi_accounts = Burn {
            mint: self.lp_mint.to_account_info().clone(),
            from: self.user_lp_token_account.to_account_info().clone(),
            authority: self.user.to_account_info().clone(),
        };
        CpiContext::new(self.token_program.to_account_info().clone(), cpi_accounts)
    }

    fn transfer_a_to_user_context(&self) -> CpiContext<'_, '_, '_, 'info, Transfer<'info>> {
        let cpi_accounts = Transfer {
            from: self.reserve_a.to_account_info().clone(),
            to: self.user_token_a.to_account_info().clone(),
            authority: self.pool.to_account_info().clone(),
        };
        CpiContext::new(self.token_program.to_account_info().clone(), cpi_accounts)
    }

    fn transfer_b_to_user_context(&self) -> CpiContext<'_, '_, '_, 'info, Transfer<'info>> {
        let cpi_accounts = Transfer {
            from: self.reserve_b.to_account_info().clone(),
            to: self.user_token_b.to_account_info().clone(),
            authority: self.pool.to_account_info().clone(),
        };
        CpiContext::new(self.token_program.to_account_info().clone(), cpi_accounts)
    }
}

#[derive(Accounts)]
pub struct Swap<'info> {
    #[account(mut, has_one = lp_mint, seeds = [b"pool", lp_mint.key().as_ref()], bump)]
    pub pool: Account<'info, Pool>,
    #[account(mut)]
    pub lp_mint: Account<'info, Mint>,
    #[account(mut, token::mint = token_a_mint)]
    pub reserve_a: Account<'info, TokenAccount>,
    #[account(mut, token::mint = token_b_mint)]
    pub reserve_b: Account<'info, TokenAccount>,

    #[account(mut)]
    pub user: Signer<'info>,
    #[account(mut, token::mint = token_a_mint, token::authority = user)]
    pub user_token_a: Account<'info, TokenAccount>,
    #[account(mut, token::mint = token_b_mint, token::authority = user)]
    pub user_token_b: Account<'info, TokenAccount>,

    /// Optional treasury token accounts (where treasury fees land)
    #[account(mut, token::mint = token_a_mint)]
    pub treasury_token_account_a: Account<'info, TokenAccount>,
    #[account(mut, token::mint = token_b_mint)]
    pub treasury_token_account_b: Account<'info, TokenAccount>,

    pub token_program: Program<'info, Token>,
    pub token_a_mint: Account<'info, Mint>,
    pub token_b_mint: Account<'info, Mint>,
}

impl<'info> Swap<'info> {
    fn transfer_in_a_context(&self) -> CpiContext<'_, '_, '_, 'info, Transfer<'info>> {
        let cpi_accounts = Transfer {
            from: self.user_token_a.to_account_info().clone(),
            to: self.reserve_a.to_account_info().clone(),
            authority: self.user.to_account_info().clone(),
        };
        CpiContext::new(self.token_program.to_account_info().clone(), cpi_accounts)
    }
    fn transfer_in_b_context(&self) -> CpiContext<'_, '_, '_, 'info, Transfer<'info>> {
        let cpi_accounts = Transfer {
            from: self.user_token_b.to_account_info().clone(),
            to: self.reserve_b.to_account_info().clone(),
            authority: self.user.to_account_info().clone(),
        };
        CpiContext::new(self.token_program.to_account_info().clone(), cpi_accounts)
    }
    fn transfer_out_a_context(&self) -> CpiContext<'_, '_, '_, 'info, Transfer<'info>> {
        let cpi_accounts = Transfer {
            from: self.reserve_a.to_account_info().clone(),
            to: self.user_token_a.to_account_info().clone(),
            authority: self.pool.to_account_info().clone(),
        };
        CpiContext::new(self.token_program.to_account_info().clone(), cpi_accounts)
    }
    fn transfer_out_b_context(&self) -> CpiContext<'_, '_, '_, 'info, Transfer<'info>> {
        let cpi_accounts = Transfer {
            from: self.reserve_b.to_account_info().clone(),
            to: self.user_token_b.to_account_info().clone(),
            authority: self.pool.to_account_info().clone(),
        };
        CpiContext::new(self.token_program.to_account_info().clone(), cpi_accounts)
    }
    fn transfer_treasury_from_reserve_a_context(&self) -> CpiContext<'_, '_, '_, 'info, Transfer<'info>> {
        let cpi_accounts = Transfer {
            from: self.reserve_a.to_account_info().clone(),
            to: self.treasury_token_account_a.to_account_info().clone(),
            authority: self.pool.to_account_info().clone(),
        };
        CpiContext::new(self.token_program.to_account_info().clone(), cpi_accounts)
    }
    fn transfer_treasury_from_reserve_b_context(&self) -> CpiContext<'_, '_, '_, 'info, Transfer<'info>> {
        let cpi_accounts = Transfer {
            from: self.reserve_b.to_account_info().clone(),
            to: self.treasury_token_account_b.to_account_info().clone(),
            authority: self.pool.to_account_info().clone(),
        };
        CpiContext::new(self.token_program.to_account_info().clone(), cpi_accounts)
    }
}

#[derive(Accounts)]
pub struct OnlyAuthority<'info> {
    #[account(mut, has_one = authority)]
    pub pool: Account<'info, Pool>,
    pub authority: Signer<'info>,
}

#[derive(Accounts)]
pub struct EmergencyWithdraw<'info> {
    #[account(mut, has_one = authority, has_one = reserve_a, has_one = reserve_b)]
    pub pool: Account<'info, Pool>,
    pub authority: Signer<'info>,
    #[account(mut, token::mint = token_a_mint)]
    pub reserve_a: Account<'info, TokenAccount>,
    #[account(mut, token::mint = token_b_mint)]
    pub reserve_b: Account<'info, TokenAccount>,
    #[account(mut, token::mint = token_a_mint)]
    pub treasury_token_account_a: Account<'info, TokenAccount>,
    #[account(mut, token::mint = token_b_mint)]
    pub treasury_token_account_b: Account<'info, TokenAccount>,
    pub token_program: Program<'info, Token>,
    pub token_a_mint: Account<'info, Mint>,
    pub token_b_mint: Account<'info, Mint>,
}

impl<'info> EmergencyWithdraw<'info> {
    fn transfer_reserve_a_to_treasury_context(&self) -> CpiContext<'_, '_, '_, 'info, Transfer<'info>> {
        let cpi_accounts = Transfer {
            from: self.reserve_a.to_account_info().clone(),
            to: self.treasury_token_account_a.to_account_info().clone(),
            authority: self.pool.to_account_info().clone(),
        };
        CpiContext::new(self.token_program.to_account_info().clone(), cpi_accounts)
    }
    fn transfer_reserve_b_to_treasury_context(&self) -> CpiContext<'_, '_, '_, 'info, Transfer<'info>> {
        let cpi_accounts = Transfer {
            from: self.reserve_b.to_account_info().clone(),
            to: self.treasury_token_account_b.to_account_info().clone(),
            authority: self.pool.to_account_info().clone(),
        };
        CpiContext::new(self.token_program.to_account_info().clone(), cpi_accounts)
    }
}

// ---------------------- Helpers ----------------------

fn calculate_lp_mint_amount(
    amount_a: u64,
    amount_b: u64,
    reserve_a: u64,
    reserve_b: u64,
    lp_supply: u64,
) -> Result<u64> {
    if lp_supply == 0 {
        let prod = u128::from(amount_a)
            .checked_mul(u128::from(amount_b))
            .ok_or(AmmError::NumericOverflow)?;
        let minted = integer_sqrt_u128(prod) as u64;
        require!(minted > 0, AmmError::InsufficientLiquidity);
        Ok(minted)
    } else {
        let supply = u128::from(lp_supply);
        let ma = u128::from(amount_a)
            .checked_mul(supply)
            .ok_or(AmmError::NumericOverflow)?
            / u128::from(reserve_a.max(1));
        let mb = u128::from(amount_b)
            .checked_mul(supply)
            .ok_or(AmmError::NumericOverflow)?
            / u128::from(reserve_b.max(1));
        let minted = core::cmp::min(ma, mb) as u64;
        require!(minted > 0, AmmError::InsufficientLiquidity);
        Ok(minted)
    }
}

fn integer_sqrt_u128(x: u128) -> u128 {
    if x <= 1 {
        return x;
    }
    let mut left: u128 = 1;
    let mut right: u128 = x;
    while left <= right {
        let mid = (left + right) / 2;
        let sq = mid.checked_mul(mid);
        match sq {
            Some(v) if v == x => return mid,
            Some(v) if v < x => left = mid + 1,
            Some(_) | None => right = mid - 1,
        }
    }
    left - 1
}

// ---------------------- Errors ----------------------

#[error_code]
pub enum AmmError {
    #[msg("Vesting period must be between min and max allowed seconds")]
    InvalidVestingPeriod,
    #[msg("Numeric overflow")]
    NumericOverflow,
    #[msg("Insufficient liquidity")]
    InsufficientLiquidity,
    #[msg("Vesting not finished yet")]
    VestingNotFinished,
    #[msg("Already claimed")]
    AlreadyClaimed,
    #[msg("Slippage exceeded")]
    SlippageExceeded,
    #[msg("Unauthorized")]
    Unauthorized,
    #[msg("Paused")]
    Paused,
    #[msg("Not rent exempt")]
    NotRentExempt,
    #[msg("Invalid token account owner")]
    InvalidTokenAccountOwner,
    #[msg("Invalid fee split")]
    InvalidFeeSplit,
    #[msg("Slot too low (anti front-run)")]
    SlotTooLow,
    #[msg("Invalid penalty")]
    InvalidPenalty,
    #[msg("Insufficient vested amount")]
    InsufficientVestedAmount,
}
