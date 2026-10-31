"use client";

import { useState } from "react";
import { useAtomValue } from "jotai/react";
import { availableModelsAtom } from "@/store/chat";
import { ChevronDown, Check } from "lucide-react";
import { cn } from "@/lib/utils";
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover";
import {
  Command,
  CommandInput,
  CommandList,
  CommandEmpty,
  CommandGroup,
  CommandItem,
} from "@/components/ui/command";

function formatModelLabel(id: string): string {
  const parts = id.split("/");
  return parts.length > 1 ? parts.slice(1).join("/") : id;
}

function formatProvider(id: string): string {
  const parts = id.split("/");
  if (parts.length > 1) return parts[0];
  return "";
}

interface ModelSelectorProps {
  value: string;
  onChange: (model: string) => void;
}

export function ModelSelector({ value, onChange }: ModelSelectorProps) {
  const [open, setOpen] = useState(false);
  const models = useAtomValue(availableModelsAtom);

  const providers = new Map<string, typeof models>();
  for (const m of models) {
    const provider = formatProvider(m.id) || "other";
    if (!providers.has(provider)) providers.set(provider, []);
    providers.get(provider)!.push(m);
  }

  return (
    <Popover open={open} onOpenChange={setOpen}>
      <PopoverTrigger asChild>
        <button
          className="flex items-center gap-0.5 text-[10px] text-muted-foreground hover:text-foreground transition-colors truncate max-w-[180px]"
          aria-label="Select model"
        >
          <span className="truncate">{formatModelLabel(value)}</span>
          <ChevronDown className="h-2.5 w-2.5 shrink-0 opacity-50" />
        </button>
      </PopoverTrigger>
      <PopoverContent className="w-64 p-0" align="start" side="top">
        <Command>
          <CommandInput placeholder="Filter models..." />
          <CommandList>
            <CommandEmpty>No models found.</CommandEmpty>
            {models.length > 0 ? (
              Array.from(providers.entries()).map(([provider, providerModels]) => (
                <CommandGroup key={provider} heading={provider}>
                  {providerModels.map((m) => (
                    <CommandItem
                      key={m.id}
                      value={m.id}
                      onSelect={() => {
                        onChange(m.id);
                        setOpen(false);
                      }}
                    >
                      <Check
                        className={cn(
                          "h-3 w-3 shrink-0",
                          value === m.id ? "opacity-100" : "opacity-0",
                        )}
                      />
                      <span className="truncate">{formatModelLabel(m.id)}</span>
                    </CommandItem>
                  ))}
                </CommandGroup>
              ))
            ) : (
              <CommandGroup>
                <CommandItem value={value} onSelect={() => setOpen(false)}>
                  <Check className="h-3 w-3 shrink-0 opacity-100" />
                  <span className="truncate">{value}</span>
                </CommandItem>
              </CommandGroup>
            )}
          </CommandList>
        </Command>
      </PopoverContent>
    </Popover>
  );
}
