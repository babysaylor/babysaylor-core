use anchor_lang::prelude::*;
use anchor_spl::{
    associated_token::AssociatedToken,
    token::{self, Mint, Token, TokenAccount, Transfer},
};

declare_id!("6BsALfjr7No2KLioFn9fS7hwaSae28W3eh9UhcMJKHLq");

// Règles métier
const MIN_PLAYERS: u64 = 15_000;
const MAX_PLAYERS: u64 = 50_000;
const LP_CAP_USDT: u64 = 1_000_000 * 1_000_000; // 1M USDT
const REGISTRATION_FEE: u64 = 100 * 1_000_000; // 100 USDT
const BSLR_ALLOCATION: u64 = 20_000 * 1_000_000; // 20k BSLR
const BSLR_BONUS: u64 = 5_000 * 1_000_000; // +25% = 5k BSLR
const BONUS_THRESHOLD: u64 = 10_000; // bonus pour les 10k premiers

#[account]
pub struct Config {
    pub admin: Pubkey,
    pub prize_pool_wallet: Pubkey,
    pub lp_wallet: Pubkey,
    pub ops_wallet: Pubkey,
    pub usdt_mint: Pubkey,
    pub bslr_mint: Pubkey,
    pub total_players: u64,
    pub registrations_open: bool,
    pub t_mid: i64,
    pub t_end: i64,
    pub total_prize_usdt: u64,
    pub total_lp_usdt: u64,
}

#[account]
pub struct PlayerData {
    pub wallet: Pubkey,
    pub bslr_allocated: u64,
    pub bonus_applied: bool,
    pub claim_initial: bool,
    pub claim_mid: bool,
    pub claim_final: bool,
}

#[event]
pub struct RegisterEvent {
    pub player: Pubkey,
    pub bslr_allocated: u64,
    pub bonus_applied: bool,
}

#[event]
pub struct ClaimEvent {
    pub player: Pubkey,
    pub amount: u64,
    pub claim_type: String,
}

#[event]
pub struct LaunchFairEvent {
    pub total_players: u64,
    pub total_prize_usdt: u64,
}

#[error_code]
pub enum ErrorCode {
    #[msg("Registrations are closed")]
    RegistrationsClosed,
    #[msg("Maximum players reached")]
    MaxPlayersReached,
    #[msg("Invalid USDT amount")]
    InvalidUsdtAmount,
    #[msg("Claim not available yet")]
    ClaimNotAvailable,
    #[msg("Already claimed")]
    AlreadyClaimed,
    #[msg("Unauthorized action")]
    Unauthorized,
    #[msg("Minimum players not reached for fair launch")]
    MinPlayersNotReached,
    #[msg("Invalid timestamps")]
    InvalidTimestamp,
}

#[program]
pub mod babysaylor_core {
    use super::*;

    pub fn initialize_config(
        ctx: Context<InitializeConfig>,
        prize_pool_wallet: Pubkey,
        lp_wallet: Pubkey,
        ops_wallet: Pubkey,
        usdt_mint: Pubkey,
        bslr_mint: Pubkey,
    ) -> Result<()> {
        let cfg = &mut ctx.accounts.config;
        cfg.admin = ctx.accounts.admin.key();
        cfg.prize_pool_wallet = prize_pool_wallet;
        cfg.lp_wallet = lp_wallet;
        cfg.ops_wallet = ops_wallet;
        cfg.usdt_mint = usdt_mint;
        cfg.bslr_mint = bslr_mint;
        cfg.total_players = 0;
        cfg.registrations_open = true;
        cfg.t_mid = 0;
        cfg.t_end = 0;
        cfg.total_prize_usdt = 0;
        cfg.total_lp_usdt = 0;
        Ok(())
    }

    pub fn register(ctx: Context<Register>, bump_auth: u8) -> Result<()> {
        let cfg = &mut ctx.accounts.config;

        require!(cfg.registrations_open, ErrorCode::RegistrationsClosed);
        require!(cfg.total_players < MAX_PLAYERS, ErrorCode::MaxPlayersReached);
        require!(
            ctx.accounts.player_usdt_account.amount >= REGISTRATION_FEE,
            ErrorCode::InvalidUsdtAmount
        );

        // 1) Le joueur paie 100 USDT vers l'escrow PDA
        token::transfer(
            CpiContext::new(
                ctx.accounts.token_program.to_account_info(),
                Transfer {
                    from: ctx.accounts.player_usdt_account.to_account_info(),
                    to: ctx.accounts.escrow_usdt_account.to_account_info(),
                    authority: ctx.accounts.player.to_account_info(),
                },
            ),
            REGISTRATION_FEE,
        )?;

        // PDA signer
        let signer_seeds: &[&[u8]] = &[b"authority", &[bump_auth]];
        let signer = &[signer_seeds];

        // Boot phase ?
        let is_boot = cfg.total_players < 1_500;
        if is_boot {
            // 80% prize, 20% ops
            let prize = REGISTRATION_FEE * 80 / 100;
            let ops = REGISTRATION_FEE * 20 / 100;

            // prize
            token::transfer(
                CpiContext::new_with_signer(
                    ctx.accounts.token_program.to_account_info(),
                    Transfer {
                        from: ctx.accounts.escrow_usdt_account.to_account_info(),
                        to: ctx.accounts.prize_pool_wallet.to_account_info(),
                        authority: ctx.accounts.program_authority.to_account_info(),
                    },
                    signer,
                ),
                prize,
            )?;
            // ops
            token::transfer(
                CpiContext::new_with_signer(
                    ctx.accounts.token_program.to_account_info(),
                    Transfer {
                        from: ctx.accounts.escrow_usdt_account.to_account_info(),
                        to: ctx.accounts.ops_wallet.to_account_info(),
                        authority: ctx.accounts.program_authority.to_account_info(),
                    },
                    signer,
                ),
                ops,
            )?;

            cfg.total_prize_usdt = cfg.total_prize_usdt.saturating_add(prize);
        } else {
            // 20% prize, 40% lp (cap), 40% ops (25%+15%) + excédent LP
            let prize = REGISTRATION_FEE * 20 / 100;
            let mut lp = REGISTRATION_FEE * 40 / 100;
            let ops_marketing = REGISTRATION_FEE * 25 / 100;
            let ops_dev = REGISTRATION_FEE * 15 / 100;

            // Cap LP
            let mut ops_extra = 0u64;
            if cfg.total_lp_usdt.saturating_add(lp) > LP_CAP_USDT {
                let room = LP_CAP_USDT.saturating_sub(cfg.total_lp_usdt);
                ops_extra = lp.saturating_sub(room);
                lp = room;
                cfg.total_lp_usdt = LP_CAP_USDT;
            } else {
                cfg.total_lp_usdt = cfg.total_lp_usdt.saturating_add(lp);
            }

            // prize
            token::transfer(
                CpiContext::new_with_signer(
                    ctx.accounts.token_program.to_account_info(),
                    Transfer {
                        from: ctx.accounts.escrow_usdt_account.to_account_info(),
                        to: ctx.accounts.prize_pool_wallet.to_account_info(),
                        authority: ctx.accounts.program_authority.to_account_info(),
                    },
                    signer,
                ),
                prize,
            )?;
            // lp
            if lp > 0 {
                token::transfer(
                    CpiContext::new_with_signer(
                        ctx.accounts.token_program.to_account_info(),
                        Transfer {
                            from: ctx.accounts.escrow_usdt_account.to_account_info(),
                            to: ctx.accounts.lp_wallet.to_account_info(),
                            authority: ctx.accounts.program_authority.to_account_info(),
                        },
                        signer,
                    ),
                    lp,
                )?;
            }
            // ops + surplus LP
            let ops_total = ops_marketing.saturating_add(ops_dev).saturating_add(ops_extra);
            token::transfer(
                CpiContext::new_with_signer(
                    ctx.accounts.token_program.to_account_info(),
                    Transfer {
                        from: ctx.accounts.escrow_usdt_account.to_account_info(),
                        to: ctx.accounts.ops_wallet.to_account_info(),
                        authority: ctx.accounts.program_authority.to_account_info(),
                    },
                    signer,
                ),
                ops_total,
            )?;

            cfg.total_prize_usdt = cfg.total_prize_usdt.saturating_add(prize);
        }

        // Attribution BSLR
        let pd = &mut ctx.accounts.player_data;
        let alloc = if cfg.total_players < BONUS_THRESHOLD {
            pd.bonus_applied = true;
            BSLR_ALLOCATION.saturating_add(BSLR_BONUS)
        } else {
            pd.bonus_applied = false;
            BSLR_ALLOCATION
        };
        pd.wallet = ctx.accounts.player.key();
        pd.bslr_allocated = alloc;
        pd.claim_initial = false;
        pd.claim_mid = false;
        pd.claim_final = false;

        // 25% initial
        token::transfer(
            CpiContext::new_with_signer(
                ctx.accounts.token_program.to_account_info(),
                Transfer {
                    from: ctx.accounts.bslr_reserve_account.to_account_info(),
                    to: ctx.accounts.player_bslr_account.to_account_info(),
                    authority: ctx.accounts.program_authority.to_account_info(),
                },
                signer,
            ),
            alloc / 4,
        )?;
        pd.claim_initial = true;

        cfg.total_players = cfg.total_players.saturating_add(1);

        emit!(RegisterEvent {
            player: ctx.accounts.player.key(),
            bslr_allocated: alloc,
            bonus_applied: pd.bonus_applied,
        });
        Ok(())
    }

    pub fn claim_mid(ctx: Context<ClaimMid>, bump_auth: u8) -> Result<()> {
        let cfg = &ctx.accounts.config;
        let pd = &mut ctx.accounts.player_data;
        let now = Clock::get()?.unix_timestamp;

        require!(now >= cfg.t_mid, ErrorCode::ClaimNotAvailable);
        require!(!pd.claim_mid, ErrorCode::AlreadyClaimed);

        let amount = pd.bslr_allocated / 4;
        let signer: &[&[&[u8]]] = &[&[b"authority", &[bump_auth]]];

        token::transfer(
            CpiContext::new_with_signer(
                ctx.accounts.token_program.to_account_info(),
                Transfer {
                    from: ctx.accounts.bslr_reserve_account.to_account_info(),
                    to: ctx.accounts.player_bslr_account.to_account_info(),
                    authority: ctx.accounts.program_authority.to_account_info(),
                },
                signer,
            ),
            amount,
        )?;
        pd.claim_mid = true;

        emit!(ClaimEvent {
            player: ctx.accounts.player.key(),
            amount,
            claim_type: "mid".to_string(),
        });
        Ok(())
    }

    pub fn claim_final(ctx: Context<ClaimFinal>, bump_auth: u8) -> Result<()> {
        let cfg = &ctx.accounts.config;
        let pd = &mut ctx.accounts.player_data;
        let now = Clock::get()?.unix_timestamp;

        require!(now >= cfg.t_end, ErrorCode::ClaimNotAvailable);
        require!(!pd.claim_final, ErrorCode::AlreadyClaimed);

        let amount = pd.bslr_allocated / 2;
        let signer: &[&[&[u8]]] = &[&[b"authority", &[bump_auth]]];

        token::transfer(
            CpiContext::new_with_signer(
                ctx.accounts.token_program.to_account_info(),
                Transfer {
                    from: ctx.accounts.bslr_reserve_account.to_account_info(),
                    to: ctx.accounts.player_bslr_account.to_account_info(),
                    authority: ctx.accounts.program_authority.to_account_info(),
                },
                signer,
            ),
            amount,
        )?;
        pd.claim_final = true;

        emit!(ClaimEvent {
            player: ctx.accounts.player.key(),
            amount,
            claim_type: "final".to_string(),
        });
        Ok(())
    }

    pub fn close_registrations(ctx: Context<CloseRegistrations>, t_mid: i64, t_end: i64) -> Result<()> {
        let cfg = &mut ctx.accounts.config;
        require!(ctx.accounts.admin.key() == cfg.admin, ErrorCode::Unauthorized);
        require!(t_mid < t_end, ErrorCode::InvalidTimestamp);
        cfg.registrations_open = false;
        cfg.t_mid = t_mid;
        cfg.t_end = t_end;
        Ok(())
    }

    pub fn launch_fair(ctx: Context<LaunchFair>) -> Result<()> {
        let cfg = &mut ctx.accounts.config;
        require!(ctx.accounts.admin.key() == cfg.admin, ErrorCode::Unauthorized);
        require!(cfg.total_players < MIN_PLAYERS, ErrorCode::MinPlayersNotReached);

        emit!(LaunchFairEvent {
            total_players: cfg.total_players,
            total_prize_usdt: cfg.total_prize_usdt,
        });

        cfg.registrations_open = false;
        Ok(())
    }
}

// ------------ Contexts ------------

#[derive(Accounts)]
pub struct InitializeConfig<'info> {
    #[account(
        init,
        payer = admin,
        space = 8 + 32 + 32 + 32 + 32 + 32 + 32 + 8 + 1 + 8 + 8 + 8 + 8
    )]
    pub config: Account<'info, Config>,
    #[account(mut)]
    pub admin: Signer<'info>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct Register<'info> {
    #[account(mut)]
    pub config: Account<'info, Config>,

    #[account(
        init,
        payer = player,
        space = 8 + 32 + 8 + 1 + 1 + 1 + 1,
        seeds = [b"player_data", player.key().as_ref()],
        bump
    )]
    pub player_data: Account<'info, PlayerData>,

    #[account(mut)]
    pub player: Signer<'info>,

    #[account(
        mut,
        constraint = player_usdt_account.mint == config.usdt_mint,
        constraint = player_usdt_account.owner == player.key()
    )]
    pub player_usdt_account: Account<'info, TokenAccount>,
    #[account(
        mut,
        constraint = player_bslr_account.mint == config.bslr_mint,
        constraint = player_bslr_account.owner == player.key()
    )]
    pub player_bslr_account: Account<'info, TokenAccount>,

    #[account(
        mut,
        associated_token::mint = usdt_mint,
        associated_token::authority = program_authority
    )]
    pub escrow_usdt_account: Account<'info, TokenAccount>,
    #[account(
        mut,
        associated_token::mint = bslr_mint,
        associated_token::authority = program_authority
    )]
    pub bslr_reserve_account: Account<'info, TokenAccount>,

    #[account(mut)]
    pub prize_pool_wallet: Account<'info, TokenAccount>,
    #[account(mut)]
    pub lp_wallet: Account<'info, TokenAccount>,
    #[account(mut)]
    pub ops_wallet: Account<'info, TokenAccount>,

    #[account(address = config.usdt_mint)]
    pub usdt_mint: Account<'info, Mint>,
    #[account(address = config.bslr_mint)]
    pub bslr_mint: Account<'info, Mint>,

    /// CHECK: PDA signer
    #[account(seeds = [b"authority"], bump)]
    pub program_authority: AccountInfo<'info>,

    pub token_program: Program<'info, Token>,
    pub associated_token_program: Program<'info, AssociatedToken>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct ClaimMid<'info> {
    #[account(mut)]
    pub config: Account<'info, Config>,
    #[account(
        mut,
        seeds = [b"player_data", player.key().as_ref()],
        bump
    )]
    pub player_data: Account<'info, PlayerData>,
    #[account(mut)]
    pub player: Signer<'info>,
    #[account(
        mut,
        constraint = player_bslr_account.mint == config.bslr_mint,
        constraint = player_bslr_account.owner == player.key()
    )]
    pub player_bslr_account: Account<'info, TokenAccount>,
    #[account(
        mut,
        associated_token::mint = bslr_mint,
        associated_token::authority = program_authority
    )]
    pub bslr_reserve_account: Account<'info, TokenAccount>,
    #[account(address = config.bslr_mint)]
    pub bslr_mint: Account<'info, Mint>,
    /// CHECK: PDA signer
    #[account(seeds = [b"authority"], bump)]
    pub program_authority: AccountInfo<'info>,
    pub token_program: Program<'info, Token>,
    pub associated_token_program: Program<'info, AssociatedToken>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct ClaimFinal<'info> {
    #[account(mut)]
    pub config: Account<'info, Config>,
    #[account(
        mut,
        seeds = [b"player_data", player.key().as_ref()],
        bump
    )]
    pub player_data: Account<'info, PlayerData>,
    #[account(mut)]
    pub player: Signer<'info>,
    #[account(
        mut,
        constraint = player_bslr_account.mint == config.bslr_mint,
        constraint = player_bslr_account.owner == player.key()
    )]
    pub player_bslr_account: Account<'info, TokenAccount>,
    #[account(
        mut,
        associated_token::mint = bslr_mint,
        associated_token::authority = program_authority
    )]
    pub bslr_reserve_account: Account<'info, TokenAccount>,
    #[account(address = config.bslr_mint)]
    pub bslr_mint: Account<'info, Mint>,
    /// CHECK: PDA signer
    #[account(seeds = [b"authority"], bump)]
    pub program_authority: AccountInfo<'info>,
    pub token_program: Program<'info, Token>,
    pub associated_token_program: Program<'info, AssociatedToken>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct CloseRegistrations<'info> {
    #[account(mut)]
    pub config: Account<'info, Config>,
    pub admin: Signer<'info>,
}

#[derive(Accounts)]
pub struct LaunchFair<'info> {
    #[account(mut)]
    pub config: Account<'info, Config>,
    pub admin: Signer<'info>,
}
