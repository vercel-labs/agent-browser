"use client";

import { atom } from "jotai";
import { useEffect } from "react";
import { useAtomCallback } from "jotai/utils";
import { useCallback } from "react";
import { Chat } from "@ai-sdk/react";
import { DefaultChatTransport, type UIMessage } from "ai";

function getChatStatusUrl(): string {
  return "/api/chat/status";
}

export function getChatApiUrl(): string {
  return "/api/chat";
}

export const CHAT_STORAGE_PREFIX = "dashboard-chat-";
const IMAGE_DATA_URL_RE = /data:image\/[^;]+;base64,[A-Za-z0-9+/=]+/g;

export function stripImagesForStorage(messages: unknown[]): unknown[] {
  const json = JSON.stringify(messages);
  return JSON.parse(json.replace(IMAGE_DATA_URL_RE, "[image stripped]"));
}

const chats = new Map<string, Chat<UIMessage>>();
let selectedChatModel: string | undefined;

/** Sets the model sent with every chat request. */
export function setChatModel(model: string | undefined) {
  selectedChatModel = model;
}

function loadMessages(session: string): UIMessage[] {
  try {
    const parsed = JSON.parse(localStorage.getItem(`${CHAT_STORAGE_PREFIX}${session}`) || "null");
    return Array.isArray(parsed) ? parsed : [];
  } catch {
    return [];
  }
}

/** Returns the long-lived chat for a session, creating it from saved messages on first use. */
export function getChat(session: string): Chat<UIMessage> {
  let chat = chats.get(session);
  if (!chat) {
    chat = new Chat<UIMessage>({
      id: session,
      messages: loadMessages(session),
      transport: new DefaultChatTransport({
        api: getChatApiUrl(),
        body: () => ({ session, model: selectedChatModel }),
      }),
      onFinish: ({ messages }) => {
        if (chats.get(session) !== chat) return;
        try {
          localStorage.setItem(
            `${CHAT_STORAGE_PREFIX}${session}`,
            JSON.stringify(stripImagesForStorage(messages)),
          );
        } catch {
          return;
        }
      },
    });
    chats.set(session, chat);
  }
  return chat;
}

/** Stops a session's running chat turn and forgets its chat and saved thread. */
export function disposeChat(session: string) {
  const chat = chats.get(session);
  chats.delete(session);
  void chat?.stop();
  try {
    localStorage.removeItem(`${CHAT_STORAGE_PREFIX}${session}`);
  } catch {
    return;
  }
}

export function getModelsApiUrl(): string {
  return "/api/models";
}

export interface ModelInfo {
  id: string;
  name?: string;
  owned_by?: string;
  context_window?: number;
}

export const chatEnabledAtom = atom(false);
export const chatModelAtom = atom<string | undefined>(undefined);
export const availableModelsAtom = atom<ModelInfo[]>([]);

export function useChatStatusSync() {
  const fetchStatus = useAtomCallback(
    useCallback(async (_get, set) => {
      try {
        const resp = await fetch(getChatStatusUrl());
        if (resp.ok) {
          const data = await resp.json();
          set(chatEnabledAtom, !!data.enabled);
          if (data.model) set(chatModelAtom, data.model);
        }
      } catch {
        set(chatEnabledAtom, false);
      }
      try {
        const resp = await fetch(getModelsApiUrl());
        if (resp.ok) {
          const data = await resp.json();
          if (Array.isArray(data?.data)) {
            const models: ModelInfo[] = data.data.map((m: Record<string, unknown>) => ({
              id: m.id as string,
              name: (m.name as string) || undefined,
              owned_by: (m.owned_by as string) || undefined,
              context_window: typeof m.context_window === "number" ? m.context_window : undefined,
            }));
            models.sort((a, b) => a.id.localeCompare(b.id));
            set(availableModelsAtom, models);
          }
        }
      } catch {
        // models fetch failed, leave empty
      }
    }, []),
  );

  useEffect(() => {
    fetchStatus();
  }, [fetchStatus]);
}
