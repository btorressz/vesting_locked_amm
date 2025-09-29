/* TODO: FIX
vesting_locked_amm - initialize / pause / unpause
    1) initialize_pool -> pause -> unpause
  0 passing (6ms)
  1 failing
  1) vesting_locked_amm - initialize / pause / unpause
       initialize_pool -> pause -> unpause:
     TypeError: Failed to fetch

*/
//This is solana playground version that is why it is named anchor.test.ts in vscode version it is // test/vesting_locked_amm.ts

import * as anchor from "@coral-xyz/anchor";
import { Program } from "@coral-xyz/anchor";
import { PublicKey, SystemProgram, Keypair, Connection } from "@solana/web3.js";
import assert from "assert";
import {
  createMint,
  getOrCreateAssociatedTokenAccount,
  TOKEN_PROGRAM_ID,
} from "@solana/spl-token";

// Playground exposes `pg` — declare it so TypeScript knows about it.
declare const pg: any;

// Use the Playground's deployed program client. This points to the IDL + program deployed in Playground.
const program = pg.program as Program;

// Anchor provider for helper utilities (we use the payer Keypair from the provider)
const provider = anchor.AnchorProvider.local();
anchor.setProvider(provider);

describe("vesting_locked_amm - initialize / pause / unpause", () => {
  it("initialize_pool -> pause -> unpause", async () => {
    const connection: Connection = provider.connection;
    const payerKeypair = (provider.wallet as any).payer as Keypair;
    const payerPubkey = payerKeypair.publicKey;

    // 1) Create SPL mints: tokenA, tokenB, lpMint
    // createMint(connection, payerKeypair, mintAuthorityPubkey, freezeAuthorityPubkey | null, decimals)
    const decimals = 6;
    const tokenA: PublicKey = await createMint(connection, payerKeypair, payerPubkey, null, decimals);
    const tokenB: PublicKey = await createMint(connection, payerKeypair, payerPubkey, null, decimals);
    const lpMint: PublicKey = await createMint(connection, payerKeypair, payerPubkey, null, decimals);

    // 2) Create reserve token accounts (ATAs) for pool reserves (owned by payer for test)
    const reserveAAccount = await getOrCreateAssociatedTokenAccount(connection, payerKeypair, tokenA, payerPubkey);
    const reserveBAccount = await getOrCreateAssociatedTokenAccount(connection, payerKeypair, tokenB, payerPubkey);

    // 3) Create treasury LP ATA to receive fees/penalties
    const treasuryLpAta = await getOrCreateAssociatedTokenAccount(connection, payerKeypair, lpMint, payerPubkey);

    // 4) Compute pool PDA (seeds: [b"pool", lp_mint.as_ref()])
    const [poolPda] = await PublicKey.findProgramAddress([Buffer.from("pool"), lpMint.toBuffer()], program.programId);

    // 5) Call initializePool
    const protocolFeeBps = 30; // example: 0.30%
    const treasuryFeeBps = 10;
    const rewardFeeBps = 20;

    // initializePool (Rust: initialize_pool) -> Anchor JS auto-camel-cases
    const tx = await program.methods
      .initializePool(protocolFeeBps, treasuryFeeBps, rewardFeeBps)
      .accounts({
        pool: poolPda,
        authority: payerPubkey,
        tokenAMint: tokenA,
        tokenBMint: tokenB,
        lpMint: lpMint,
        reserveA: reserveAAccount.address,
        reserveB: reserveBAccount.address,
        treasury: treasuryLpAta.address,
        tokenProgram: TOKEN_PROGRAM_ID,
        systemProgram: SystemProgram.programId,
        rent: anchor.web3.SYSVAR_RENT_PUBKEY,
      })
      // payer is creating the pool / is current mint authority in test
      .signers([payerKeypair])
      .rpc();

    console.log("initialize_pool tx:", tx);
    await connection.confirmTransaction(tx, "confirmed");

    // 6) Fetch the Pool account and assert contents
    const poolAccount = (await program.account.pool.fetch(poolPda)) as any;
    assert.ok(poolAccount, "Pool account should exist");

    // Convert on-chain pubkey-like values to PublicKey to call toBase58()
    const lpMintOnChain = new PublicKey(poolAccount.lpMint as string);
    const tokenAOnChain = new PublicKey(poolAccount.tokenAMint as string);
    const tokenBOnChain = new PublicKey(poolAccount.tokenBMint as string);
    const treasuryOnChain = new PublicKey(poolAccount.treasury as string);

    assert.equal(lpMintOnChain.toBase58(), lpMint.toBase58());
    assert.equal(tokenAOnChain.toBase58(), tokenA.toBase58());
    assert.equal(tokenBOnChain.toBase58(), tokenB.toBase58());
    assert.equal(treasuryOnChain.toBase58(), treasuryLpAta.address.toBase58());
    assert.equal(poolAccount.protocolFeeBps, protocolFeeBps);
    assert.equal(poolAccount.treasuryFeeBps, treasuryFeeBps);
    assert.equal(poolAccount.rewardFeeBps, rewardFeeBps);

    // vestingNonce may be a BN-like; handle safely
    const vestingNonceNum =
      poolAccount.vestingNonce && typeof (poolAccount.vestingNonce as any).toNumber === "function"
        ? (poolAccount.vestingNonce as any).toNumber()
        : Number(poolAccount.vestingNonce);
    assert.equal(vestingNonceNum, 0);
    assert.equal(poolAccount.paused, false);

    // 7) Pause the pool (authority-only)
    const txPause = await program.methods
      .pause()
      .accounts({
        pool: poolPda,
        authority: payerPubkey,
      })
      .rpc();
    await connection.confirmTransaction(txPause, "confirmed");

    const poolAfterPause = (await program.account.pool.fetch(poolPda)) as any;
    assert.equal(poolAfterPause.paused, true, "Pool should be paused after pause()");

    // 8) Unpause the pool
    const txUnpause = await program.methods
      .unpause()
      .accounts({
        pool: poolPda,
        authority: payerPubkey,
      })
      .rpc();
    await connection.confirmTransaction(txUnpause, "confirmed");

    const poolAfterUnpause = (await program.account.pool.fetch(poolPda)) as any;
    assert.equal(poolAfterUnpause.paused, false, "Pool should be unpaused after unpause()");

    console.log("initialize -> pause -> unpause test passed");
  }).timeout(90_000);
});
