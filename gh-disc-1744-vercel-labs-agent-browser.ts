// src/lib/endpoints.ts
import { NextRequest, NextResponse } from 'next/server';

// Define types for the paid x402 endpoints
interface PaidEndpointConfig {
  path: string;
  method: 'GET' | 'POST';
  handler: (req: NextRequest) => Promise<NextResponse>;
}

// Helper to validate x402 payment headers
const validateX402Payment = (req: NextRequest): boolean => {
  const paymentHeader = req.headers.get('x402-payment');
  return !!paymentHeader && paymentHeader.startsWith('x402-');
};

// Helper to return 402 Payment Required response
const paymentRequired = (message: string = 'Payment required'): NextResponse => {
  return NextResponse.json(
    { error: 'Payment Required', message },
    { status: 402, headers: { 'x402-reason': 'unpaid' } }
  );
};

// 11 paid x402 endpoints
const paidEndpoints: PaidEndpointConfig[] = [
  {
    path: '/api/ai/generate',
    method: 'POST',
    handler: async (req) => {
      if (!validateX402Payment(req)) return paymentRequired();
      const { prompt } = await req.json();
      if (!prompt) return NextResponse.json({ error: 'Missing prompt' }, { status: 400 });
      // Simulate AI generation (replace with actual implementation)
      return NextResponse.json({ result: `Generated response for: ${prompt}` });
    },
  },
  {
    path: '/api/ai/analyze',
    method: 'POST',
    handler: async (req) => {
      if (!validateX402Payment(req)) return paymentRequired();
      const { data } = await req.json();
      if (!data) return NextResponse.json({ error: 'Missing data' }, { status: 400 });
      return NextResponse.json({ analysis: `Analyzed ${data.length} items` });
    },
  },
  {
    path: '/api/data/export',
    method: 'GET',
    handler: async (req) => {
      if (!validateX402Payment(req)) return paymentRequired();
      const { format } = req.nextUrl.searchParams;
      if (!format) return NextResponse.json({ error: 'Missing format' }, { status: 400 });
      return NextResponse.json({ export: `Data exported in ${format} format` });
    },
  },
  {
    path: '/api/data/import',
    method: 'POST',
    handler: async (req) => {
      if (!validateX402Payment(req)) return paymentRequired();
      const { file } = await req.json();
      if (!file) return NextResponse.json({ error: 'Missing file' }, { status: 400 });
      return NextResponse.json({ imported: true, count: 100 });
    },
  },
  {
    path: '/api/ai/chat',
    method: 'POST',
    handler: async (req) => {
      if (!validateX402Payment(req)) return paymentRequired();
      const { messages } = await req.json();
      if (!messages || !Array.isArray(messages)) return NextResponse.json({ error: 'Invalid messages' }, { status: 400 });
      return NextResponse.json({ reply: `Processed ${messages.length} messages` });
    },
  },
  {
    path: '/api/ai/translate',
    method: 'POST',
    handler: async (req) => {
      if (!validateX402Payment(req)) return paymentRequired();
      const { text, targetLang } = await req.json();
      if (!text || !targetLang) return NextResponse.json({ error: 'Missing text or targetLang' }, { status: 400 });
      return NextResponse.json({ translation: `Translated to ${targetLang}` });
    },
  },
  {
    path: '/api/ai/summarize',
    method: 'POST',
    handler: async (req) => {
      if (!validateX402Payment(req)) return paymentRequired();
      const { content } = await req.json();
      if (!content) return NextResponse.json({ error: 'Missing content' }, { status: 400 });
      return NextResponse.json({ summary: `Summarized ${content.length} chars` });
    },
  },
  {
    path: '/api/ai/classify',
    method: 'POST',
    handler: async (req) => {
      if (!validateX402Payment(req)) return paymentRequired();
      const { items } = await req.json();
      if (!items || !Array.isArray(items)) return NextResponse.json({ error: 'Invalid items' }, { status: 400 });
      return NextResponse.json({ classifications: items.map(() => 'classified') });
    },
  },
  {
    path: '/api/ai/extract',
    method: 'POST',
    handler: async (req) => {
      if (!validateX402Payment(req)) return paymentRequired();
      const { text, type } = await req.json();
      if (!text || !type) return NextResponse.json({ error: 'Missing text or type' }, { status: 400 });
      return NextResponse.json({ extracted: `Extracted ${type} from text` });
    },
  },
  {
    path: '/api/ai/generate-image',
    method: 'POST',
    handler: async (req) => {
      if (!validateX402Payment(req)) return paymentRequired();
      const { prompt } = await req.json();
      if (!prompt) return NextResponse.json({ error: 'Missing prompt' }, { status: 400 });
      return NextResponse.json({ imageUrl: 'https://example.com/image.png' });
    },
  },
  {
    path: '/api/ai/voice',
    method: 'POST',
    handler: async (req) => {
      if (!validateX402Payment(req)) return paymentRequired();
      const { text } = await req.json();
      if (!text) return NextResponse.json({ error: 'Missing text' }, { status: 400 });
      return NextResponse.json({ audioUrl: 'https://example.com/audio.mp3' });
    },
  },
];

// Export handler for all endpoints
export const dynamic = 'force-dynamic';

export async function GET(req: NextRequest) {
  const url = new URL(req.url);
  const endpoint = paidEndpoints.find(e => e.path === url.pathname && e.method === 'GET');
  if (endpoint) return endpoint.handler(req);
  return NextResponse.json({ error: 'Not found' }, { status: 404 });
}

export async function POST(req: NextRequest) {
  const url = new URL(req.url);
  const endpoint = paidEndpoints.find(e => e.path === url.pathname && e.method === 'POST');
  if (endpoint) return endpoint.handler(req);
  return NextResponse.json({ error: 'Not found' }, { status: 404 });
}