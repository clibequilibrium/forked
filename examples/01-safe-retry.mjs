// Use case 1, safe retry after a failed agent run.
//
// A travel-booking agent charges a card, books a flight and emails the
// customer, then crashes on a flaky step. We re-run the WHOLE agent from
// scratch, the dangerous pattern that normally double-charges people.
// With forked's effect log, every already-performed side effect is skipped
// and its stored result reused; only the remaining work runs.
//
//   cargo run -p forked-server -- --db memory --addr 127.0.0.1:8080
//   node 01-safe-retry.mjs

import assert from "node:assert/strict";
import { ForkedClient } from "@forked-run/client";

const forked = new ForkedClient({
  baseUrl: process.env.FORKED_URL ?? "http://127.0.0.1:8080",
  apiKey: process.env.FORKED_API_KEY,
});

// ---------------------------------------------------------------------------
// The "outside world". Each function has a real, irreversible consequence -
// these counters stand in for Stripe, Amadeus and SendGrid.
// ---------------------------------------------------------------------------
const world = { charges: [], bookings: [], emails: [] };

const stripe = async (amount) => {
  world.charges.push(amount);
  return { txn: `txn_${world.charges.length}`, amount };
};
const airline = async (flight) => {
  world.bookings.push(flight);
  return { pnr: `PNR${100 + world.bookings.length}`, flight };
};
const sendgrid = async (to) => {
  world.emails.push(to);
  return { messageId: `msg_${world.emails.length}` };
};

// ---------------------------------------------------------------------------
// The effectively-once recipe: probe the effect log, work only on a miss, then
// record the result. On a re-run, recorded steps return instantly with the
// original result.
// ---------------------------------------------------------------------------
async function performOnce(session, { key, kind, payload }, work) {
  const existing = await session.findEffect(key, { payload });
  if (existing) {
    console.log(`    ↩ ${kind}, already done, reusing ${JSON.stringify(existing.result)}`);
    return existing.result;
  }
  const result = await work();
  console.log(`    ⚡ ${kind}, EXECUTED → ${JSON.stringify(result)}`);
  const outcome = await session.recordEffect({ key, kind, payload, result });
  return outcome.record.result; // covers a lost race: use whoever won
}

// ---------------------------------------------------------------------------
// The agent. Attempt #1 dies on a flaky loyalty-points API *after* the
// money moved and the email went out.
// ---------------------------------------------------------------------------
async function runTripAgent(session, { flakyStepWorks }) {
  const main = session.branch();

  console.log("  step 1: plan the trip (pure state, no side effects)");
  await main.put(
    { task: "book BER→LIS for u-77", flight: "TP533", amount: 184 },
    { meta: { message: "planned trip" } },
  );

  const charge = await performOnce(
    session,
    { key: "charge:u-77:TP533", kind: "payment.charge", payload: { amount: 184 } },
    () => stripe(184),
  );
  await main.put({ charge }, { meta: { message: "charged card" } });

  const booking = await performOnce(
    session,
    { key: "book:u-77:TP533", kind: "flight.book", payload: { flight: "TP533" } },
    () => airline("TP533"),
  );
  await main.put({ booking }, { meta: { message: "booked flight" } });

  const email = await performOnce(
    session,
    { key: "email:u-77:confirmation", kind: "email.send", payload: { to: "u-77@example.com" } },
    () => sendgrid("u-77@example.com"),
  );
  await main.put({ email }, { meta: { message: "sent confirmation" } });

  console.log("  step 5: credit loyalty points (the flaky one)");
  if (!flakyStepWorks) {
    throw new Error("loyalty-points API: 504 Gateway Timeout");
  }
  await main.put({ loyaltyCredited: true, status: "complete" }, { meta: { message: "done" } });
}

// ---------------------------------------------------------------------------
const session = await forked.createSession();
console.log(`session: ${session.id}\n`);

console.log("── attempt #1 ──────────────────────────────────────────");
try {
  await runTripAgent(session, { flakyStepWorks: false });
} catch (e) {
  console.log(`  ✗ CRASHED: ${e.message}`);
}
console.log(
  `  world after crash: ${world.charges.length} charge, ${world.bookings.length} booking, ${world.emails.length} email\n`,
);

console.log("── attempt #2: naive full re-run of the same agent ─────");
await runTripAgent(session, { flakyStepWorks: true });

console.log("\n── outcome ─────────────────────────────────────────────");
console.log(
  `  charges: ${world.charges.length}  bookings: ${world.bookings.length}  emails: ${world.emails.length}  (all exactly once)`,
);
assert.equal(world.charges.length, 1, "card must be charged exactly once");
assert.equal(world.bookings.length, 1, "flight must be booked exactly once");
assert.equal(world.emails.length, 1, "email must be sent exactly once");
assert.equal((await session.branch().state()).state.status, "complete");
console.log("  final state: status=complete ✓");
console.log(`\n✓ retried the whole agent, side effects ran exactly once (session ${session.id})`);
