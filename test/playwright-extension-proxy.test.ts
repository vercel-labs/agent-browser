import { describe, it, expect, beforeEach, afterEach, vi } from 'vitest';
import { PlaywrightExtensionProxy } from '../src/playwright-extension-proxy.js';
import { WebSocket } from 'ws';

describe('PlaywrightExtensionProxy', () => {
  let proxy: PlaywrightExtensionProxy;

  beforeEach(async () => {
    proxy = new PlaywrightExtensionProxy();
  });

  afterEach(async () => {
    await proxy.stop();
  });

  it('should start and provide relay and CDP URLs', async () => {
    const info = await proxy.start();
    expect(info.relayUrl).toMatch(/^ws:\/\/127.0.0.1:\d+$/);
    expect(info.cdpUrl).toMatch(/^ws:\/\/127.0.0.1:\d+$/);
  });

  it('should handle handshake with extension', async () => {
    const info = await proxy.start();
    
    const extensionSocket = new WebSocket(info.relayUrl);
    
    // Mock extension side
    extensionSocket.on('message', (data) => {
      const msg = JSON.parse(data.toString());
      if (msg.method === 'attachToTab') {
        extensionSocket.send(JSON.stringify({
          id: msg.id,
          result: {
            targetInfo: { title: 'Test Page', url: 'https://example.com' },
            sessionId: 'session-123'
          }
        }));
      }
    });

    const readyPromise = new Promise<any>((resolve) => {
      proxy.once('extension-ready', resolve);
    });

    const targetInfo = await readyPromise;
    expect(targetInfo.title).toBe('Test Page');
    expect(targetInfo.url).toBe('https://example.com');
    
    extensionSocket.close();
  });

  it('should translate CDP commands from Playwright to Extension', async () => {
    const info = await proxy.start();
    
    const extensionSocket = new WebSocket(info.relayUrl);
    
    // Handshake
    extensionSocket.on('message', (data) => {
      const msg = JSON.parse(data.toString());
      if (msg.method === 'attachToTab') {
        extensionSocket.send(JSON.stringify({
          id: msg.id,
          result: { targetInfo: {}, sessionId: 's1' }
        }));
      } else if (msg.method === 'forwardCDPCommand') {
        // Echo back with result
        expect(msg.params.method).toBe('Page.navigate');
        expect(msg.params.params.url).toBe('https://example.com');
        extensionSocket.send(JSON.stringify({
          id: msg.id,
          result: { frameId: 'f1' }
        }));
      }
    });

    await new Promise(r => proxy.once('extension-ready', r));

    const playwrightSocket = new WebSocket(info.cdpUrl);
    
    await new Promise(r => playwrightSocket.on('open', r));

    const responsePromise = new Promise<any>((resolve) => {
      playwrightSocket.on('message', (data) => {
        const msg = JSON.parse(data.toString());
        if (msg.id === 100) resolve(msg);
      });
    });

    playwrightSocket.send(JSON.stringify({
      id: 100,
      method: 'Page.navigate',
      params: { url: 'https://example.com' }
    }));

    const resp = await responsePromise;
    expect(resp.id).toBe(100);
    expect(resp.result.frameId).toBe('f1');

    playwrightSocket.close();
    extensionSocket.close();
  });

  it('should unwrapp CDP events from Extension to Playwright', async () => {
    const info = await proxy.start();
    
    const extensionSocket = new WebSocket(info.relayUrl);
    
    // Handshake
    extensionSocket.on('message', (data) => {
      const msg = JSON.parse(data.toString());
      if (msg.method === 'attachToTab') {
        extensionSocket.send(JSON.stringify({
          id: msg.id,
          result: { targetInfo: {}, sessionId: 's1' }
        }));
      }
    });

    await new Promise(r => proxy.once('extension-ready', r));

    const playwrightSocket = new WebSocket(info.cdpUrl);
    await new Promise(r => playwrightSocket.on('open', r));

    const eventPromise = new Promise<any>((resolve) => {
      playwrightSocket.on('message', (data) => {
        const msg = JSON.parse(data.toString());
        if (msg.method === 'Console.messageAdded') resolve(msg);
      });
    });

    // Send event from extension
    extensionSocket.send(JSON.stringify({
      method: 'forwardCDPEvent',
      params: {
        sessionId: 's1',
        method: 'Console.messageAdded',
        params: { message: 'hello' }
      }
    }));

    const event = await eventPromise;
    expect(event.method).toBe('Console.messageAdded');
    expect(event.params.message).toBe('hello');

    playwrightSocket.close();
    extensionSocket.close();
  });

  it('should intercept Target.getTargetInfo', async () => {
    const info = await proxy.start();
    
    const extensionSocket = new WebSocket(info.relayUrl);
    
    // Handshake
    extensionSocket.on('message', (data) => {
      const msg = JSON.parse(data.toString());
      if (msg.method === 'attachToTab') {
        extensionSocket.send(JSON.stringify({
          id: msg.id,
          result: { targetInfo: { targetId: 't1' }, sessionId: 's1' }
        }));
      }
    });

    await new Promise(r => proxy.once('extension-ready', r));

    const playwrightSocket = new WebSocket(info.cdpUrl);
    await new Promise(r => playwrightSocket.on('open', r));

    const responsePromise = new Promise<any>((resolve) => {
      playwrightSocket.on('message', (data) => {
        const msg = JSON.parse(data.toString());
        if (msg.id === 200) resolve(msg);
      });
    });

    playwrightSocket.send(JSON.stringify({
      id: 200,
      method: 'Target.getTargetInfo',
      params: {}
    }));

    const resp = await responsePromise;
    expect(resp.id).toBe(200);
    expect(resp.result.targetInfo.targetId).toBe('t1');

    playwrightSocket.close();
    extensionSocket.close();
  });
});
