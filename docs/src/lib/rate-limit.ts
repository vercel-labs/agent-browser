import { Ratelimit } from "@upstash/ratelimit";
import { Redis } from "@upstash/redis";

// Lazy initialization to avoid errors when Redis env vars are not configured
let _minuteRateLimit: Ratelimit | null = null;
let _dailyRateLimit: Ratelimit | null = null;

function getRedis(): Redis | null {
  const url = process.env.KV_REST_API_URL;
  const token = process.env.KV_REST_API_TOKEN;

  if (!url || !token) {
    return null;
  }

  return new Redis({ url, token });
}

// No-op rate limiter for when Redis is not configured
const noopRateLimiter = {
  limit: async () => ({ success: true, limit: 0, remaining: 0, reset: 0 }),
};

const MINUTE_LIMIT = Number(process.env.RATE_LIMIT_PER_MINUTE) || 10;
const DAILY_LIMIT = Number(process.env.RATE_LIMIT_PER_DAY) || 100;

// Requests per minute (sliding window)
export const minuteRateLimit = {
  limit: async (identifier: string) => {
    if (!_minuteRateLimit) {
      const redis = getRedis();
      if (!redis) return noopRateLimiter.limit();
      _minuteRateLimit = new Ratelimit({
        redis,
        limiter: Ratelimit.slidingWindow(MINUTE_LIMIT, "1 m"),
        prefix: "ratelimit:minute",
      });
    }
    return _minuteRateLimit.limit(identifier);
  },
};

// Requests per day (fixed window)
export const dailyRateLimit = {
  limit: async (identifier: string) => {
    if (!_dailyRateLimit) {
      const redis = getRedis();
      if (!redis) return noopRateLimiter.limit();
      _dailyRateLimit = new Ratelimit({
        redis,
        limiter: Ratelimit.fixedWindow(DAILY_LIMIT, "1 d"),
        prefix: "ratelimit:daily",
      });
    }
    return _dailyRateLimit.limit(identifier);
  },
};
