export interface FrameMessage {
  type: "frame";
  data: string;
  metadata: {
    offsetTop: number;
    pageScaleFactor: number;
    deviceWidth: number;
    deviceHeight: number;
    scrollOffsetX: number;
    scrollOffsetY: number;
    timestamp: number;
  };
}

export interface StatusMessage {
  type: "status";
  connected: boolean;
  screencasting: boolean;
  viewportWidth: number;
  viewportHeight: number;
  engine?: string;
  recording?: boolean;
}

export interface CommandMessage {
  type: "command";
  action: string;
  id: string;
  params: Record<string, unknown>;
  timestamp: number;
}

export interface ResultMessage {
  type: "result";
  id: string;
  action: string;
  success: boolean;
  data: unknown;
  duration_ms: number;
  timestamp: number;
}

export interface ConsoleMessage {
  type: "console";
  level: string;
  text: string;
  timestamp: number;
}

export interface UrlMessage {
  type: "url";
  url: string;
  timestamp: number;
}

export interface PageErrorMessage {
  type: "page_error";
  text: string;
  line: number | null;
  column: number | null;
  timestamp: number;
}

export interface ErrorMessage {
  type: "error";
  message: string;
}

export interface TabInfo {
  index: number;
  title: string;
  url: string;
  type: string;
  active: boolean;
}

export interface TabsMessage {
  type: "tabs";
  tabs: TabInfo[];
  timestamp: number;
}

export type StreamMessage =
  | FrameMessage
  | StatusMessage
  | CommandMessage
  | ResultMessage
  | ConsoleMessage
  | PageErrorMessage
  | ErrorMessage
  | UrlMessage
  | TabsMessage;

export type ActivityEvent = CommandMessage | ResultMessage | ConsoleMessage;
export type ConsoleEntry = ConsoleMessage | PageErrorMessage;

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
