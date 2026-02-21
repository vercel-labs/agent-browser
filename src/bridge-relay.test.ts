import { describe, it, expect } from 'vitest';
import { BridgeCdpShim, type CdpMessage, type RelayMessage } from './bridge-relay.js';

function setupShim() {
  const cdpOut: Array<Record<string, unknown>> = [];
  const relayOut: Array<Record<string, unknown>> = [];

  const shim = new BridgeCdpShim({
    sendToCdp: (message) => cdpOut.push(message),
    sendToRelay: (message) => {
      relayOut.push(message);
      return true;
    },
  });

  return { shim, cdpOut, relayOut };
}

async function attachToTab(shim: BridgeCdpShim, relayOut: Array<Record<string, unknown>>) {
  const pending = shim.handleCdpMessage({
    id: 1,
    method: 'Target.setAutoAttach',
    params: { autoAttach: true },
  } as CdpMessage);

  const attachRequest = relayOut[0] as RelayMessage;
  expect(attachRequest.method).toBe('attachToTab');

  await shim.handleRelayMessage({
    id: attachRequest.id,
    result: {
      targetInfo: {
        targetId: 'tab-1',
        type: 'page',
        title: 'Example',
        url: 'https://example.com',
      },
    },
  } as RelayMessage);

  await pending;
}

describe('BridgeCdpShim', () => {
  it('maps Target.setAutoAttach to attachToTab and emits attachedToTarget', async () => {
    const { shim, cdpOut, relayOut } = setupShim();

    await attachToTab(shim, relayOut);

    expect(cdpOut[0]).toMatchObject({
      method: 'Target.attachedToTarget',
      params: {
        waitingForDebugger: false,
        targetInfo: {
          targetId: 'tab-1',
          type: 'page',
          title: 'Example',
          url: 'https://example.com',
          attached: true,
        },
      },
    });
    expect(cdpOut[0].params).toHaveProperty('sessionId', 'pw-tab-1');
    expect(cdpOut[1]).toEqual({ id: 1, result: {} });
  });

  it('forwards CDP commands with session mapping and returns original response id', async () => {
    const { shim, cdpOut, relayOut } = setupShim();
    await attachToTab(shim, relayOut);

    relayOut.length = 0;
    cdpOut.length = 0;

    const pending = shim.handleCdpMessage({
      id: 23,
      method: 'Runtime.evaluate',
      sessionId: 'pw-tab-1',
      params: { expression: '1+1' },
    } as CdpMessage);

    const command = relayOut[0] as RelayMessage;
    expect(command.method).toBe('forwardCDPCommand');
    expect(command.params).toEqual({
      sessionId: undefined,
      method: 'Runtime.evaluate',
      params: { expression: '1+1' },
    });

    await shim.handleRelayMessage({
      id: command.id,
      result: { result: { type: 'number', value: 2 } },
    } as RelayMessage);
    await pending;

    expect(cdpOut[0]).toEqual({
      id: 23,
      sessionId: 'pw-tab-1',
      result: { result: { type: 'number', value: 2 } },
    });
  });

  it('translates forwardCDPEvent into standard CDP event frames', async () => {
    const { shim, cdpOut } = setupShim();
    await shim.handleRelayMessage({
      method: 'forwardCDPEvent',
      params: {
        sessionId: 'relay-session',
        method: 'Runtime.consoleAPICalled',
        params: { type: 'log' },
      },
    } as RelayMessage);

    expect(cdpOut[0]).toEqual({
      sessionId: 'relay-session',
      method: 'Runtime.consoleAPICalled',
      params: { type: 'log' },
    });
  });

  it('handles Browser.getVersion with stub response and no relay traffic', async () => {
    const { shim, cdpOut, relayOut } = setupShim();

    await shim.handleCdpMessage({
      id: 50,
      method: 'Browser.getVersion',
      params: {},
    } as CdpMessage);

    expect(relayOut).toHaveLength(0);
    expect(cdpOut[0]).toMatchObject({
      id: 50,
      result: {
        protocolVersion: '1.3',
      },
    });
  });

  it('handles Browser.setDownloadBehavior as noop with no relay traffic', async () => {
    const { shim, cdpOut, relayOut } = setupShim();

    await shim.handleCdpMessage({
      id: 51,
      method: 'Browser.setDownloadBehavior',
      params: {},
    } as CdpMessage);

    expect(relayOut).toHaveLength(0);
    expect(cdpOut[0]).toEqual({ id: 51, result: {} });
  });

  it('returns cached target info for Target.getTargetInfo', async () => {
    const { shim, cdpOut, relayOut } = setupShim();
    await attachToTab(shim, relayOut);

    cdpOut.length = 0;
    await shim.handleCdpMessage({
      id: 99,
      method: 'Target.getTargetInfo',
      params: {},
    } as CdpMessage);

    expect(cdpOut[0]).toEqual({
      id: 99,
      result: {
        targetInfo: {
          targetId: 'tab-1',
          type: 'page',
          title: 'Example',
          url: 'https://example.com',
        },
      },
    });
  });

  it('rejects pending relay commands when extension disconnects', async () => {
    const { shim, cdpOut, relayOut } = setupShim();
    await attachToTab(shim, relayOut);

    relayOut.length = 0;
    cdpOut.length = 0;

    const pending = shim.handleCdpMessage({
      id: 120,
      method: 'Runtime.evaluate',
      sessionId: 'pw-tab-1',
      params: { expression: '2+2' },
    } as CdpMessage);

    expect((relayOut[0] as RelayMessage).method).toBe('forwardCDPCommand');

    shim.notifyRelayDisconnected('Bridge extension disconnected');
    await pending;

    expect(cdpOut[0]).toEqual({
      id: 120,
      sessionId: 'pw-tab-1',
      error: { message: 'Bridge extension disconnected' },
    });
  });

  it('fails fast when relay is not connected', async () => {
    const cdpOut: Array<Record<string, unknown>> = [];
    const shim = new BridgeCdpShim({
      sendToCdp: (message) => cdpOut.push(message),
      sendToRelay: () => false,
    });

    await shim.handleCdpMessage({
      id: 121,
      method: 'Runtime.evaluate',
      params: { expression: '3+3' },
    } as CdpMessage);

    expect(cdpOut[0]).toEqual({
      id: 121,
      error: { message: 'Bridge extension is not connected' },
    });
  });
});
