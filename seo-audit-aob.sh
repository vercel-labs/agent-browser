#!/usr/bin/env bash
# ============================================================
#  SEO Audit — alchemyofbreath.com
#  Run this on your Mac terminal where agent-browser is installed
#  Usage: bash seo-audit-aob.sh
# ============================================================

set -euo pipefail

SITE="https://alchemyofbreath.com"
REPORT="aob-seo-report.md"
SITEMAP_FILE="/tmp/aob-sitemap-urls.txt"

echo "# SEO Audit — alchemyofbreath.com" > "$REPORT"
echo "Generated: $(date)" >> "$REPORT"
echo "" >> "$REPORT"

# ─── Helper: extract SEO data from current page ────────────────────────────
audit_page() {
  local url="$1"
  local label="$2"

  echo ""
  echo "━━━ Auditing: $url ━━━"

  agent-browser open "$url" > /dev/null 2>&1
  agent-browser wait --load networkidle > /dev/null 2>&1

  # Grab all SEO data in one eval call
  local data
  data=$(agent-browser eval "
    (() => {
      const get = (sel, attr) => {
        const el = document.querySelector(sel);
        return el ? (attr ? el.getAttribute(attr) : el.textContent.trim()) : null;
      };
      const getAll = (sel, attr) => {
        return Array.from(document.querySelectorAll(sel)).map(el =>
          attr ? el.getAttribute(attr) : el.textContent.trim()
        );
      };
      return JSON.stringify({
        url:           location.href,
        title:         document.title,
        metaDesc:      get('meta[name=\"description\"]', 'content'),
        metaRobots:    get('meta[name=\"robots\"]', 'content'),
        canonical:     get('link[rel=\"canonical\"]', 'href'),
        h1:            getAll('h1'),
        h2:            getAll('h2').slice(0, 8),
        ogTitle:       get('meta[property=\"og:title\"]', 'content'),
        ogDesc:        get('meta[property=\"og:description\"]', 'content'),
        ogImage:       get('meta[property=\"og:image\"]', 'content'),
        twitterCard:   get('meta[name=\"twitter:card\"]', 'content'),
        twitterTitle:  get('meta[name=\"twitter:title\"]', 'content'),
        structuredData: Array.from(document.querySelectorAll('script[type=\"application/ld+json\"]')).map(s => {
          try { return JSON.parse(s.textContent); } catch(e) { return null; }
        }).filter(Boolean).map(d => d['@type']),
        llmsTxt:       null
      });
    })()
  " 2>/dev/null)

  # Parse and write to report
  echo "" >> "$REPORT"
  echo "---" >> "$REPORT"
  echo "" >> "$REPORT"
  echo "## $label" >> "$REPORT"
  echo "**URL:** \`$url\`" >> "$REPORT"
  echo "" >> "$REPORT"

  node -e "
    const d = JSON.parse($'$data');
    const lines = [];
    // Index status
    const robots = d.metaRobots || 'not set (defaults to index, follow)';
    const isNoindex = robots.includes('noindex');
    const isNofollow = robots.includes('nofollow');
    lines.push('### Index / Crawl Status');
    lines.push('| Field | Value |');
    lines.push('|-------|-------|');
    lines.push('| Meta Robots | \`' + robots + '\` |');
    lines.push('| Indexable | ' + (isNoindex ? '❌ NOINDEX' : '✅ Indexed') + ' |');
    lines.push('| Followable | ' + (isNofollow ? '🚫 NOFOLLOW' : '✅ Follow') + ' |');
    lines.push('| Canonical | ' + (d.canonical || '⚠️ Not set') + ' |');
    lines.push('');
    lines.push('### On-Page SEO');
    lines.push('| Field | Value |');
    lines.push('|-------|-------|');
    lines.push('| Title | ' + (d.title || '❌ Missing') + ' |');
    lines.push('| Title Length | ' + (d.title ? d.title.length + ' chars' : 'n/a') + ' |');
    lines.push('| Meta Description | ' + (d.metaDesc || '❌ Missing') + ' |');
    lines.push('| Meta Desc Length | ' + (d.metaDesc ? d.metaDesc.length + ' chars' : 'n/a') + ' |');
    lines.push('');
    lines.push('### H1 Tags (' + d.h1.length + ')');
    if (d.h1.length === 0) lines.push('❌ No H1 found');
    else if (d.h1.length > 1) lines.push('⚠️ Multiple H1s — should be exactly one');
    d.h1.forEach((h, i) => lines.push((i+1) + '. ' + h));
    lines.push('');
    lines.push('### H2 Tags (first ' + d.h2.length + ')');
    d.h2.forEach((h, i) => lines.push((i+1) + '. ' + h));
    lines.push('');
    lines.push('### Social / Open Graph');
    lines.push('| Field | Value |');
    lines.push('|-------|-------|');
    lines.push('| OG Title | ' + (d.ogTitle || '❌ Missing') + ' |');
    lines.push('| OG Description | ' + (d.ogDesc || '❌ Missing') + ' |');
    lines.push('| OG Image | ' + (d.ogImage ? '✅ Set' : '❌ Missing') + ' |');
    lines.push('| Twitter Card | ' + (d.twitterCard || '❌ Missing') + ' |');
    lines.push('');
    lines.push('### Structured Data (@type)');
    if (d.structuredData.length === 0) lines.push('⚠️ No structured data found');
    else d.structuredData.forEach(t => lines.push('- ' + t));
    console.log(lines.join('\n'));
  " >> "$REPORT" 2>/dev/null || echo "⚠️ Could not parse data for $url" >> "$REPORT"

  echo "  ✓ Done"
}

# ─── Step 1: Fetch sitemap ──────────────────────────────────────────────────
echo ""
echo "📋 Step 1: Fetching sitemap..."
echo "" >> "$REPORT"
echo "---" >> "$REPORT"
echo "" >> "$REPORT"
echo "## Sitemap" >> "$REPORT"

agent-browser open "$SITE/sitemap.xml" > /dev/null 2>&1
agent-browser wait --load networkidle > /dev/null 2>&1

SITEMAP_CONTENT=$(agent-browser get html "body" 2>/dev/null || agent-browser eval "document.body.innerText" 2>/dev/null)

# Check for sitemap index (multiple sitemaps)
agent-browser eval "
  const text = document.body.innerText || document.documentElement.innerText;
  const urls = [];
  const locRegex = /<loc>(.*?)<\/loc>/gs;
  let match;
  const raw = document.documentElement.innerHTML;
  while ((match = locRegex.exec(raw)) !== null) {
    urls.push(match[1].trim());
  }
  console.log(urls.join('\n'));
" 2>/dev/null > "$SITEMAP_FILE" || true

URL_COUNT=$(wc -l < "$SITEMAP_FILE" | tr -d ' ')
echo "Found **$URL_COUNT URLs** in sitemap." >> "$REPORT"
echo "" >> "$REPORT"
echo '```' >> "$REPORT"
cat "$SITEMAP_FILE" >> "$REPORT"
echo '```' >> "$REPORT"

echo "  ✓ Found $URL_COUNT URLs"

# ─── Step 2: Check robots.txt ───────────────────────────────────────────────
echo ""
echo "🤖 Step 2: Checking robots.txt..."
echo "" >> "$REPORT"
echo "---" >> "$REPORT"
echo "" >> "$REPORT"
echo "## robots.txt" >> "$REPORT"
echo "" >> "$REPORT"

agent-browser open "$SITE/robots.txt" > /dev/null 2>&1
ROBOTS=$(agent-browser eval "document.body.innerText" 2>/dev/null || echo "Not found")
echo '```' >> "$REPORT"
echo "$ROBOTS" >> "$REPORT"
echo '```' >> "$REPORT"
echo "  ✓ Done"

# ─── Step 3: Check for llms.txt ─────────────────────────────────────────────
echo ""
echo "🤖 Step 3: Checking llms.txt (LLM protocol)..."
echo "" >> "$REPORT"
echo "---" >> "$REPORT"
echo "" >> "$REPORT"
echo "## llms.txt (LLM Protocol)" >> "$REPORT"
echo "" >> "$REPORT"

agent-browser open "$SITE/llms.txt" > /dev/null 2>&1
HTTP_STATUS=$(agent-browser eval "document.title" 2>/dev/null)
LLMS_CONTENT=$(agent-browser eval "document.body.innerText" 2>/dev/null || echo "")

if echo "$LLMS_CONTENT" | grep -qi "404\|not found\|page not found"; then
  echo "❌ **llms.txt not found** — this site has not implemented the LLM protocol." >> "$REPORT"
  echo "" >> "$REPORT"
  echo "### Recommendation" >> "$REPORT"
  echo "Add \`/llms.txt\` to help AI crawlers understand your site structure." >> "$REPORT"
else
  echo "✅ **llms.txt found**" >> "$REPORT"
  echo '```' >> "$REPORT"
  echo "$LLMS_CONTENT" >> "$REPORT"
  echo '```' >> "$REPORT"
fi
echo "  ✓ Done"

# ─── Step 4: Audit key pages ────────────────────────────────────────────────
echo ""
echo "🔍 Step 4: Auditing pages..."
echo "" >> "$REPORT"
echo "---" >> "$REPORT"
echo "" >> "$REPORT"
echo "## Page-by-Page SEO Audit" >> "$REPORT"

audit_page "$SITE/" "Homepage"
audit_page "$SITE/breathwork-training/" "Breathwork Training"
audit_page "$SITE/breathwork-fundamentals-course/" "Breathwork Fundamentals Course"
audit_page "$SITE/events/" "Events"
audit_page "$SITE/blog/" "Blog"
audit_page "$SITE/shop/" "Shop"
audit_page "$SITE/free-breathwork-sessions/" "Free Breathwork Sessions"
audit_page "$SITE/free-anxiety-masterclass/" "Free Anxiety Masterclass"
audit_page "$SITE/asha-retreats/" "ASHA Retreats"
audit_page "$SITE/breathcamps/" "BreathCamps"
audit_page "$SITE/contact/" "Contact"

# ─── Step 5: Summary ────────────────────────────────────────────────────────
echo "" >> "$REPORT"
echo "---" >> "$REPORT"
echo "" >> "$REPORT"
echo "## Summary & Recommendations" >> "$REPORT"
echo "" >> "$REPORT"
echo "### llms.txt Protocol" >> "$REPORT"
echo "The [llms.txt standard](https://llmstxt.org) is a \`/llms.txt\` file at the site root that:" >> "$REPORT"
echo "- Tells AI crawlers (ChatGPT, Perplexity, Claude) what your site is about" >> "$REPORT"
echo "- Lists key pages with descriptions for AI indexing" >> "$REPORT"
echo "- Optionally provides a \`/llms-full.txt\` with complete site content for LLMs" >> "$REPORT"
echo "" >> "$REPORT"
echo "**Minimum viable llms.txt for alchemyofbreath.com:**" >> "$REPORT"
cat >> "$REPORT" << 'LLMS'
```markdown
# Alchemy of Breath

> Breathwork training, courses, retreats, and events. World-class facilitator
> training and free resources for breathwork practitioners and students.

## Core Pages

- [Home](https://alchemyofbreath.com/): Breathwork training and events hub
- [Breathwork Training](https://alchemyofbreath.com/breathwork-training/): Professional facilitator certification programs
- [Events](https://alchemyofbreath.com/events/): Live breathwork sessions and retreats
- [ASHA Retreats](https://alchemyofbreath.com/asha-retreats/): Immersive retreat experiences
- [BreathCamps](https://alchemyofbreath.com/breathcamps/): Multi-day breathwork intensives
- [Shop](https://alchemyofbreath.com/shop/): Courses and downloads

## Free Resources

- [Free Breathwork Sessions](https://alchemyofbreath.com/free-breathwork-sessions/): No-cost guided sessions
- [Free Anxiety Masterclass](https://alchemyofbreath.com/free-anxiety-masterclass/): Breathwork for anxiety relief
- [Blog](https://alchemyofbreath.com/blog/): Articles on breathwork, wellness, and practice

## Optional

- [llms-full.txt](https://alchemyofbreath.com/llms-full.txt)
```
LLMS

echo "" >> "$REPORT"
echo "---" >> "$REPORT"
echo "_Report complete. Open \`aob-seo-report.md\` to review._" >> "$REPORT"

echo ""
echo "✅ Audit complete! Report saved to: $(pwd)/$REPORT"
echo "   Open it with: open $REPORT"
agent-browser close > /dev/null 2>&1 || true
