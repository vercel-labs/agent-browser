#!/usr/bin/env node

/**
 * create-stripe-products.js
 *
 * One-time setup script: creates agent-browser Pro, Team, and Enterprise
 * products and prices in your Stripe account, then prints the price IDs and
 * hosted payment links to copy into your environment / payment portal.
 *
 * Requirements:
 *   STRIPE_SECRET_KEY environment variable (sk_live_... or sk_test_...)
 *
 * Usage:
 *   STRIPE_SECRET_KEY=sk_test_... node scripts/create-stripe-products.js
 *   STRIPE_SECRET_KEY=sk_test_... node scripts/create-stripe-products.js --dry-run
 */

import Stripe from 'stripe'

const DRY_RUN = process.argv.includes('--dry-run')

const SECRET_KEY = process.env.STRIPE_SECRET_KEY
if (!SECRET_KEY) {
  console.error('Error: STRIPE_SECRET_KEY environment variable is required.')
  console.error('Usage: STRIPE_SECRET_KEY=sk_... node scripts/create-stripe-products.js')
  process.exit(1)
}

const stripe = new Stripe(SECRET_KEY, { apiVersion: '2025-12-15.clover' })

// ─── Product definitions ──────────────────────────────────────────────────────

const PRODUCTS = [
  {
    name: 'agent-browser Pro',
    description:
      'Headless browser automation CLI — Pro plan. Includes video recording, parallel sessions, cloud provider integration, and 2,000 req/h rate limit.',
    metadata: { plan: 'pro' },
    prices: [
      {
        nickname: 'Pro Monthly',
        currency: 'usd',
        unit_amount: 2900, // $29.00
        recurring: { interval: 'month' },
        metadata: { plan: 'pro', interval: 'monthly' },
      },
      {
        nickname: 'Pro Annual',
        currency: 'usd',
        unit_amount: 28900, // $289.00/year (~17% off)
        recurring: { interval: 'year' },
        metadata: { plan: 'pro', interval: 'annual' },
      },
    ],
  },
  {
    name: 'agent-browser Team',
    description:
      'Headless browser automation CLI — Team plan. Includes all Pro features plus 5 seats, team dashboard, audit log, and Slack support.',
    metadata: { plan: 'team' },
    prices: [
      {
        nickname: 'Team Monthly',
        currency: 'usd',
        unit_amount: 7900, // $79.00
        recurring: { interval: 'month' },
        metadata: { plan: 'team', interval: 'monthly' },
      },
      {
        nickname: 'Team Annual',
        currency: 'usd',
        unit_amount: 78900, // $789.00/year (~17% off)
        recurring: { interval: 'year' },
        metadata: { plan: 'team', interval: 'annual' },
      },
    ],
  },
]

// ─── Webhook endpoint definition ──────────────────────────────────────────────

const WEBHOOK_EVENTS = [
  'checkout.session.completed',
  'customer.subscription.created',
  'customer.subscription.updated',
  'customer.subscription.deleted',
  'invoice.paid',
  'invoice.payment_failed',
]

// ─── Main ─────────────────────────────────────────────────────────────────────

async function main() {
  console.log(`agent-browser Stripe setup ${DRY_RUN ? '(DRY RUN)' : ''}`)
  console.log('─'.repeat(50))

  const results = []

  for (const productDef of PRODUCTS) {
    console.log(`\nProduct: ${productDef.name}`)

    let product
    if (!DRY_RUN) {
      product = await stripe.products.create({
        name: productDef.name,
        description: productDef.description,
        metadata: productDef.metadata,
      })
      console.log(`  Created product: ${product.id}`)
    } else {
      product = { id: 'prod_dry_run_' + productDef.metadata.plan }
      console.log(`  [dry-run] Would create product: ${product.id}`)
    }

    const priceIds = []
    for (const priceDef of productDef.prices) {
      let price
      if (!DRY_RUN) {
        price = await stripe.prices.create({
          product: product.id,
          currency: priceDef.currency,
          unit_amount: priceDef.unit_amount,
          recurring: priceDef.recurring,
          nickname: priceDef.nickname,
          metadata: priceDef.metadata,
        })
        console.log(`  Created price (${priceDef.nickname}): ${price.id}`)
      } else {
        price = { id: 'price_dry_run_' + priceDef.nickname.toLowerCase().replace(/\s+/g, '_') }
        console.log(`  [dry-run] Would create price (${priceDef.nickname}): ${price.id}`)
      }

      // Create a hosted payment link for each price
      let paymentLink
      if (!DRY_RUN) {
        paymentLink = await stripe.paymentLinks.create({
          line_items: [{ price: price.id, quantity: 1 }],
          allow_promotion_codes: true,
          after_completion: {
            type: 'redirect',
            redirect: { url: 'https://agentbrowser.dev/pro/success?session_id={CHECKOUT_SESSION_ID}' },
          },
        })
        console.log(`  Payment link (${priceDef.nickname}): ${paymentLink.url}`)
      } else {
        paymentLink = { url: 'https://buy.stripe.com/dry_run_link', id: 'plink_dry_run' }
        console.log(`  [dry-run] Would create payment link: ${paymentLink.url}`)
      }

      priceIds.push({
        nickname: priceDef.nickname,
        priceId: price.id,
        paymentLink: paymentLink.url,
        unitAmount: priceDef.unit_amount,
        interval: priceDef.recurring.interval,
      })
    }

    results.push({
      plan: productDef.metadata.plan,
      productId: product.id,
      prices: priceIds,
    })
  }

  // ── Summary ──────────────────────────────────────────────────────────────────

  console.log('\n' + '═'.repeat(50))
  console.log('SETUP COMPLETE — copy these values to your environment:\n')

  for (const r of results) {
    console.log(`# ${r.plan.toUpperCase()} plan (product: ${r.productId})`)
    for (const p of r.prices) {
      const envKey = `STRIPE_PRICE_${r.plan.toUpperCase()}_${p.interval.toUpperCase()}`
      console.log(`${envKey}=${p.priceId}`)
    }
    console.log('')
  }

  console.log('# Payment links (share these with customers):')
  for (const r of results) {
    for (const p of r.prices) {
      console.log(`# ${r.plan} ${p.interval}: ${p.paymentLink}`)
    }
  }

  console.log('\n# Webhook events to subscribe (Stripe Dashboard → Webhooks):')
  WEBHOOK_EVENTS.forEach((e) => console.log(`#   ${e}`))

  console.log('\n# Webhook endpoint URL:')
  console.log('#   https://agentbrowser.dev/api/stripe/webhook')

  if (!DRY_RUN) {
    console.log('\nRemember to set STRIPE_WEBHOOK_SECRET after creating the webhook endpoint.')
  }
}

main().catch((err) => {
  console.error('\nError:', err.message)
  if (err.type === 'StripeAuthenticationError') {
    console.error('Check that STRIPE_SECRET_KEY is correct.')
  }
  process.exit(1)
})
