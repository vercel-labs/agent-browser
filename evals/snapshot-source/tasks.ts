// The tasks of the snapshot-source A/B eval: 20 from v1 and 6 held-out ones added for v2. Each task names a fixture page, a user prompt and
// a check that the harness runs against recorded page events and the browser URL.

import type { RunEvent } from "./fixtures.ts";

export type TaskCategory = "form" | "iframe" | "spa" | "heavy" | "trap";

export interface CheckEnv {
  events: RunEvent[];
  url: string;
}

export interface Task {
  id: string;
  category: TaskCategory;
  page: string;
  prompt: string;
  /** Held out: written before the v2 implementation, not used to tune it. */
  holdout?: boolean;
  check(env: CheckEnv): boolean;
}

const of = (events: RunEvent[], t: string) => events.filter((e) => e.t === t);
const only = (events: RunEvent[], t: string) => {
  const list = of(events, t);
  return list.length === 1 ? list[0].d : undefined;
};
const same = (a: unknown, b: unknown) => JSON.stringify(a) === JSON.stringify(b);

export const tasks: Task[] = [
  // ---- forms ----
  {
    id: "form-signup",
    category: "form",
    page: "signup",
    prompt: "Create an account for Ada Lovelace (ada@example.com) on the Pro plan and subscribe her to the newsletter.",
    check: ({ events }) => same(only(events, "submit"), { name: "Ada Lovelace", email: "ada@example.com", plan: "pro", newsletter: true }),
  },
  {
    id: "form-radio-label",
    category: "form",
    page: "shipping",
    prompt: "Place an order with Express delivery to postal code 94107 and the delivery instructions \"Leave at door\".",
    check: ({ events }) => same(only(events, "submit"), { speed: "express", zip: "94107", notes: "Leave at door" }),
  },
  {
    id: "form-wizard",
    category: "form",
    page: "wizard",
    prompt: "Create a workspace with the work email dev@acme.io and the password Sup3rSecret!. Accept the terms.",
    check: ({ events }) => same(only(events, "submit"), { email: "dev@acme.io", password: "Sup3rSecret!", agree: true }),
  },
  {
    id: "form-placeholders",
    category: "form",
    page: "route",
    prompt: "Search flights from Lima to Buenos Aires for 2 passengers.",
    check: ({ events }) => same(only(events, "submit"), { from: "Lima", to: "Buenos Aires", pax: "2" }),
  },
  {
    id: "form-disabled-submit",
    category: "form",
    page: "terms",
    prompt: "Register as Grace. Accept the terms but do not sign up for promotions.",
    check: ({ events }) => same(only(events, "submit"), { name: "Grace", accept: true, promo: false }),
  },

  // ---- iframes ----
  {
    id: "iframe-same-origin",
    category: "iframe",
    page: "frame-same",
    prompt: "Confirm the shipment.",
    check: ({ events }) => of(events, "confirm").length === 1,
  },
  {
    id: "iframe-nested-same-origin",
    category: "iframe",
    page: "frame-deep",
    prompt: "Save the note \"ship it\".",
    check: ({ events }) => of(events, "note").some((e) => e.d === "ship it"),
  },
  {
    id: "iframe-cross-origin",
    category: "iframe",
    page: "frame-cross",
    prompt: "Pay for the order with the card number 4242424242424242.",
    check: ({ events }) => of(events, "pay").some((e) => e.d === "4242424242424242" && e.host === "127.0.0.1"),
  },
  {
    id: "iframe-cross-origin-nested",
    category: "iframe",
    page: "frame-cross-deep",
    prompt: "Approve the request in the embedded widget.",
    check: ({ events }) => of(events, "approve").length >= 1,
  },
  {
    id: "iframe-shadow-dom",
    category: "iframe",
    page: "shadow",
    prompt: "Apply the coupon SAVE10.",
    check: ({ events }) => same(only(events, "activate"), { coupon: "SAVE10" }),
  },

  // ---- SPA and div-button apps ----
  {
    id: "spa-div-buttons",
    category: "spa",
    page: "cart-spa",
    prompt: "Add two apples and one pear to the cart, then check out.",
    check: ({ events }) => same(only(events, "checkout"), { apple: 2, pear: 1 }),
  },
  {
    id: "spa-role-widgets",
    category: "spa",
    page: "settings-spa",
    prompt: "Turn on email alerts under Notifications and save the changes.",
    check: ({ events }) => same(only(events, "save"), { email: true }),
  },
  {
    id: "spa-cookie-overlay",
    category: "spa",
    page: "overlay",
    prompt: "Reject all cookies, then subscribe to the newsletter.",
    check: ({ events }) => same(events.map((e) => `${e.t}:${e.d}`), ["cookies:reject", "subscribe:1"]),
  },
  {
    id: "spa-custom-dropdown",
    category: "spa",
    page: "dropdown",
    prompt: "Sort the catalog by price from low to high.",
    check: ({ events }) => same(only(events, "sort"), "price-asc"),
  },

  // ---- heavy pages ----
  {
    id: "heavy-docs-link",
    category: "heavy",
    page: "docs-heavy",
    prompt: "Open the documentation page titled exactly \"Configure retry policy\".",
    check: ({ url }) => url.endsWith("/docs-retry"),
  },
  {
    id: "heavy-orders-table",
    category: "heavy",
    page: "orders",
    prompt: "Archive order ORD-7342.",
    check: ({ events }) => same(events.map((e) => `${e.t}:${e.d}`), ["archive:ORD-7342"]),
  },
  {
    id: "heavy-product-grid",
    category: "heavy",
    page: "products",
    prompt: "Add the Quartz Desk Lamp to the cart.",
    check: ({ events }) => same(events.map((e) => `${e.t}:${e.d}`), ["add:Quartz Desk Lamp"]),
  },

  // ---- hidden-content traps ----
  {
    id: "trap-ghost-button",
    category: "trap",
    page: "ghost",
    prompt: "Send a message from hi@x.co using the contact form.",
    check: ({ events }) => same(events.map((e) => `${e.t}:${e.d}`), ["send:hi@x.co"]),
  },
  {
    id: "trap-collapsed-section",
    category: "trap",
    page: "accordion",
    prompt: "Download the invoice.",
    check: ({ events }) => of(events, "invoice").length === 1,
  },
  {
    id: "trap-covered-button",
    category: "trap",
    page: "covered",
    prompt: "Delete the project without signing up for the newsletter.",
    check: ({ events }) => same(events.map((e) => `${e.t}:${e.d}`), ["popup:dismissed", "delete:1"]),
  },

  // ---- held-out tasks (v2) ----
  {
    id: "holdout-iframe-form",
    category: "iframe",
    holdout: true,
    page: "frame-form",
    prompt: "Send a contact request for Mia Chen (mia@example.com) about Support.",
    check: ({ events }) => same(only(events, "contact"), { name: "Mia Chen", email: "mia@example.com", topic: "support" }),
  },
  {
    id: "holdout-iframe-nested-select",
    category: "iframe",
    holdout: true,
    page: "frame-deep-b",
    prompt: "Set the paper size to Large and apply it.",
    check: ({ events }) => same(only(events, "size"), "l"),
  },
  {
    id: "holdout-iframe-cross-button",
    category: "iframe",
    holdout: true,
    page: "frame-cross-button",
    prompt: "Subscribe to the newsletter.",
    check: ({ events }) => events.length === 1 && events[0].t === "subscribe" && events[0].host === "127.0.0.1",
  },
  {
    id: "holdout-iframe-heavy",
    category: "iframe",
    holdout: true,
    page: "frame-heavy",
    prompt: "Rotate the key for the ledger service.",
    check: ({ events }) => same(events.map((e) => `${e.t}:${e.d}`), ["rotate:ledger"]),
  },
  {
    id: "holdout-form-survey",
    category: "form",
    holdout: true,
    page: "survey",
    prompt: "Send feedback with the rating Poor, visit type Online and the comment \"Slow checkout\". Do not allow contact.",
    check: ({ events }) => same(only(events, "submit"), { rating: "bad", visit: "Online", comments: "Slow checkout", contact: false }),
  },
  {
    id: "holdout-spa-file-menu",
    category: "spa",
    holdout: true,
    page: "files-menu",
    prompt: "Rename report.pdf to q3-report.pdf.",
    check: ({ events }) => same(events.map((e) => `${e.t}:${JSON.stringify(e.d)}`), ['rename:{"from":"report.pdf","to":"q3-report.pdf"}']),
  },
];
