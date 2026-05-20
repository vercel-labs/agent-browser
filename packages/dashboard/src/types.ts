export type {
  ActivityEvent,
  CommandMessage,
  ConsoleEntry,
  ConsoleMessage,
  CursorKeyword,
  CursorMessage,
  ErrorMessage,
  FrameMessage,
  PageErrorMessage,
  ResultMessage,
  StatusMessage,
  StreamMessage,
  TabInfo,
  TabsMessage,
  UrlMessage,
} from "@agent-browser/client";

export interface ExtensionInfo {
  name: string;
  version: string;
  description?: string;
  path: string;
}

export interface SessionInfo {
  session: string;
  port: number;
  engine?: string;
  provider?: string;
  extensions?: ExtensionInfo[];
  pending?: boolean;
  closing?: boolean;
}
