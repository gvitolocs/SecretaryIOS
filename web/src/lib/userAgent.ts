// A User-Agent string as a person reads it ("Safari", "macOS"), for the QR sign-in approve screen.
// Only the common browsers and systems; anything else comes back as null.

export interface UaSummary {
  browser: string | null;
  os: string | null;
}

export function describeUserAgent(ua: string): UaSummary {
  return { browser: browserOf(ua), os: osOf(ua) };
}

function browserOf(ua: string): string | null {
  if (/\bEdg(A|iOS)?\//.test(ua)) return "Edge";
  if (/\b(OPR|OPiOS)\//.test(ua)) return "Opera";
  if (/\b(Firefox|FxiOS)\//.test(ua)) return "Firefox";
  if (/\bSamsungBrowser\//.test(ua)) return "Samsung Internet";
  if (/\b(HeadlessChrome|Chrome|CriOS|Chromium)\//.test(ua)) return "Chrome";
  if (/\bVersion\/[\d.]+.*\bSafari\//.test(ua)) return "Safari";
  // WKWebView (the Secretary apps' embedded views) has AppleWebKit without the Safari token.
  if (/\bAppleWebKit\//.test(ua) && !/\bSafari\//.test(ua)) return "Secretary app";
  return null;
}

function osOf(ua: string): string | null {
  if (/\biPhone\b/.test(ua)) return "iOS";
  if (/\biPad\b/.test(ua)) return "iPadOS";
  if (/\bAndroid\b/.test(ua)) return "Android";
  if (/\bCrOS\b/.test(ua)) return "ChromeOS";
  if (/\bMac OS X\b|\bMacintosh\b/.test(ua)) return "macOS";
  if (/\bWindows\b/.test(ua)) return "Windows";
  if (/\bLinux\b/.test(ua)) return "Linux";
  return null;
}
