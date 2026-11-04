import type { ProductContent, Product } from "@/components/landing/product-context";
import type { AgentUseCasesProps } from "@/components/landing/agent-use-cases";
import type { AgentTweetWallProps } from "@/components/landing/agent-tweet-wall";

const home = {
  "metadata": {
    "title": "agent-browser | Browser Automation for AI",
    "description": "Browser automation CLI for AI agents"
  },
  "hero": {
    "defaultProduct": "browser",
    "browser": {
      "headline": "Browser automation built for agents",
      "description": "Give your agent a browser. It can test your app, grab a screenshot, and check its work with the tools you already use.",
      "docsHref": "/docs",
      "prompt": "Read the agent-browser docs: https://agent-browser.dev/"
    },
    "desktop": {
      "headline": "Desktop automation built for agents",
      "description": "Give your agent the desktop. It can use native apps, click through menus, and finish the job when it leaves the browser.",
      "docsHref": "https://agent-browser.dev/desktop",
      "prompt": "Read the agent-desktop docs: https://agent-browser.dev/desktop"
    }
  },
  "useCases": {
    "heading": "Let your agent act, verify, and continue its work without you",
    "items": [
      {
        "title": "Sign in once, stay signed in",
        "visual": "sign-in",
        "description": "Keep **logins and cookies** across runs, so your agent can pick up where it left off."
      },
      {
        "title": "Run performance tests",
        "visual": "performance",
        "description": "Measure **Core Web Vitals**, profile a reload to find what’s slow, and measure again after the fix."
      },
      {
        "title": "Take screenshots and recordings",
        "visual": "verify",
        "description": "Capture screenshots to check your agent’s work. Record **video at up to 60 fps** when the motion matters."
      },
      {
        "title": "Read only what matters",
        "visual": "snapshots",
        "description": "Use **compact snapshots** of page controls, or map a window with agent-desktop and inspect only the region you need."
      },
      {
        "title": "Mock and inspect the network",
        "visual": "network",
        "description": "Inspect requests, block scripts, or **mock API responses** to reach the states your app rarely hits."
      },
      {
        "title": "Run agents side by side",
        "visual": "sessions",
        "description": "Run agents in **separate browser sessions**, or coordinate them on one desktop with agent-desktop, each with its own cursor."
      }
    ]
  },
  "tweets": {
    "heading": "The open source tools developers give their agents",
    "posts": [
      {
        "id": "2040067262124601358",
        "excerpt": [
          "Agent-browser is the best CLI tool I have given to my agents",
          "It barely consumes any tokens compared to things like playwright"
        ],
        "name": "0xSero",
        "handle": "0xSero",
        "avatar": "https://pbs.twimg.com/profile_images/2111226379521892352/3ICUbYY6_200x200.jpg"
      },
      {
        "id": "2011522613991129265",
        "excerpt": [
          "okay, so @vercel's new agent browser is pretty stinkin great",
          "substantially faster than playwright and is clearly optimized for AI use"
        ],
        "name": "Josh Pigford",
        "handle": "Shpigford",
        "avatar": "https://pbs.twimg.com/profile_images/2010446308608290816/w6Bt7Vgc_200x200.jpg"
      },
      {
        "id": "2011643006840160744",
        "excerpt": [
          "The agent browser is very good. So good that I replaced it immediately in my compound engineering plugin"
        ],
        "name": "Kieran Klaassen",
        "handle": "kieranklaassen",
        "avatar": "https://pbs.twimg.com/profile_images/1826620508789485568/AfWTPhxT_200x200.jpg"
      },
      {
        "id": "2011021383451754886",
        "excerpt": [
          "Vercel's agent-browser is wild",
          "70% less token consumption compared with Chrome dev tool MCP in my test"
        ],
        "name": "Jason Zhou",
        "handle": "jasonzhou1993",
        "avatar": "https://pbs.twimg.com/profile_images/1613651966663749632/AuQiWkVc_200x200.jpg"
      },
      {
        "id": "2013625824549969964",
        "excerpt": [
          "I am no longer using _any_ MCP servers in my local setup",
          "Playwright → SKILL + agent-browser"
        ],
        "name": "Eleanor Berger",
        "handle": "intellectronica",
        "avatar": "https://pbs.twimg.com/profile_images/1998656086987980800/mTwCcf33_200x200.jpg"
      },
      {
        "id": "2013235204488360125",
        "excerpt": [
          "if you want your agent to do browser testing, you have to try agent-browser",
          "so fast and token efficient"
        ],
        "name": "Ryan Carson",
        "handle": "ryancarson",
        "avatar": "https://pbs.twimg.com/profile_images/2111981537365196800/k2HE9uOo_200x200.jpg"
      },
      {
        "id": "2032740310036463801",
        "excerpt": [
          "Don’t sleep on agent-browser",
          "It’s a massive unlock"
        ],
        "name": "Fabrizio Rinaldi",
        "handle": "linuz90",
        "avatar": "https://pbs.twimg.com/profile_images/2091494080328941568/HJrX9sjv_200x200.jpg"
      },
      {
        "id": "2032700396955697218",
        "excerpt": [
          "If you are not using agent-browser to close the loop on frontend, you are missing out"
        ],
        "name": "Onur Solmaz",
        "handle": "onusoz",
        "avatar": "https://pbs.twimg.com/profile_images/1998282426783875072/AD1wK-0c_200x200.jpg"
      }
    ]
  },
  "closing": {
    "headline": "Your agent can write code,\nnow let it do the rest"
  }
} satisfies {
  metadata: { title: string; description: string };
  hero: { defaultProduct: Product; browser: ProductContent; desktop: ProductContent };
  useCases: AgentUseCasesProps;
  tweets: AgentTweetWallProps;
  closing: { headline: string };
};

export default home;
