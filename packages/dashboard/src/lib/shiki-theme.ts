import type { ThemeRegistrationAny } from "streamdown";

const lightTheme: ThemeRegistrationAny = {
  name: "dashboard-light",
  type: "light",
  colors: {
    "editor.background": "transparent",
    "editor.foreground": "#171717",
  },
  settings: [
    {
      scope: ["comment", "punctuation.definition.comment"],
      settings: { foreground: "#6B7280" },
    },
    {
      scope: [
        "string",
        "string.quoted",
        "string.template",
        "punctuation.definition.string",
      ],
      settings: { foreground: "#067A6E" },
    },
    {
      scope: [
        "constant.numeric",
        "constant.language.boolean",
        "constant.language.null",
      ],
      settings: { foreground: "#0070C0" },
    },
    {
      scope: ["keyword", "storage.type", "storage.modifier"],
      settings: { foreground: "#D6409F" },
    },
    {
      scope: ["keyword.operator", "keyword.control"],
      settings: { foreground: "#D6409F" },
    },
    {
      scope: ["entity.name.function", "support.function", "meta.function-call"],
      settings: { foreground: "#6E56CF" },
    },
    {
      scope: ["variable", "variable.other"],
      settings: { foreground: "#171717" },
    },
    {
      scope: ["variable.parameter"],
      settings: { foreground: "#B45309" },
    },
    {
      scope: ["entity.name.tag", "support.class.component", "entity.name.type"],
      settings: { foreground: "#D6409F" },
    },
    {
      scope: ["punctuation", "meta.brace", "meta.bracket"],
      settings: { foreground: "#6B7280" },
    },
    {
      scope: [
        "support.type.property-name",
        "entity.name.tag.json",
        "meta.object-literal.key",
        "punctuation.support.type.property-name",
      ],
      settings: { foreground: "#D6409F" },
    },
    {
      scope: ["entity.other.attribute-name"],
      settings: { foreground: "#067A6E" },
    },
    {
      scope: ["support.type.primitive", "entity.name.type.primitive"],
      settings: { foreground: "#067A6E" },
    },
  ],
};

const darkTheme: ThemeRegistrationAny = {
  name: "dashboard-dark",
  type: "dark",
  colors: {
    "editor.background": "transparent",
    "editor.foreground": "#EDEDED",
  },
  settings: [
    {
      scope: ["comment", "punctuation.definition.comment"],
      settings: { foreground: "#A1A1A1" },
    },
    {
      scope: [
        "string",
        "string.quoted",
        "string.template",
        "punctuation.definition.string",
      ],
      settings: { foreground: "#00CA50" },
    },
    {
      scope: [
        "constant.numeric",
        "constant.language.boolean",
        "constant.language.null",
      ],
      settings: { foreground: "#47A8FF" },
    },
    {
      scope: ["keyword", "storage.type", "storage.modifier"],
      settings: { foreground: "#FF4D8D" },
    },
    {
      scope: ["keyword.operator", "keyword.control"],
      settings: { foreground: "#FF4D8D" },
    },
    {
      scope: ["entity.name.function", "support.function", "meta.function-call"],
      settings: { foreground: "#C472FB" },
    },
    {
      scope: ["variable", "variable.other"],
      settings: { foreground: "#EDEDED" },
    },
    {
      scope: ["variable.parameter"],
      settings: { foreground: "#FF9300" },
    },
    {
      scope: ["entity.name.tag", "support.class.component", "entity.name.type"],
      settings: { foreground: "#FF4D8D" },
    },
    {
      scope: ["punctuation", "meta.brace", "meta.bracket"],
      settings: { foreground: "#EDEDED" },
    },
    {
      scope: [
        "support.type.property-name",
        "entity.name.tag.json",
        "meta.object-literal.key",
        "punctuation.support.type.property-name",
      ],
      settings: { foreground: "#FF4D8D" },
    },
    {
      scope: ["entity.other.attribute-name"],
      settings: { foreground: "#00CA50" },
    },
    {
      scope: ["support.type.primitive", "entity.name.type.primitive"],
      settings: { foreground: "#00CA50" },
    },
  ],
};

export const shikiTheme: [ThemeRegistrationAny, ThemeRegistrationAny] = [
  lightTheme,
  darkTheme,
];
