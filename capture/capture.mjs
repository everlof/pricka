import { chromium } from "playwright";
import fs from "fs";
import path from "path";

const BANKS = {
  "handelsbanken-privat": "https://www.handelsbanken.se/sv/",
  "handelsbanken-foretag": "https://secure.handelsbanken.se/logon/se/corp/sv/",
  "seb-foretag": "https://seb.se/foretag",
  "seb-privat": "https://seb.se/privat",
};

const args = process.argv.slice(2);
const flags = args.filter((a) => a.startsWith("--"));
const positional = args.filter((a) => !a.startsWith("--"));

const bankName = positional[0];
const proxyUrl = flags.includes("--proxy")
  ? positional[1] || "socks5://127.0.0.1:9050"
  : flags.find((f) => f.startsWith("--proxy="))?.split("=")[1];

if (!bankName || !BANKS[bankName]) {
  console.error("Usage: node capture.mjs <bank> [--proxy[=URL]]");
  console.error(`Available banks: ${Object.keys(BANKS).join(", ")}`);
  process.exit(1);
}

const startUrl = BANKS[bankName];
const captured = [];
let requestId = 0;

async function main() {
  const launchOptions = { headless: false };

  if (proxyUrl) {
    launchOptions.proxy = { server: proxyUrl };
    console.log(`Using proxy: ${proxyUrl}`);
  }

  console.log(`Opening ${bankName} (${startUrl})...`);
  console.log("Navigate manually: log in, view accounts, transactions, etc.");
  console.log("Press Ctrl+C or close the browser when done.\n");

  const browser = await chromium.launch(launchOptions);
  const context = await browser.newContext({
    locale: "sv-SE",
    viewport: { width: 1280, height: 900 },
  });

  const page = await context.newPage();

  // Intercept all requests
  page.on("request", (request) => {
    const id = ++requestId;
    request._captureId = id;

    captured.push({
      id,
      type: "request",
      timestamp: new Date().toISOString(),
      method: request.method(),
      url: request.url(),
      resourceType: request.resourceType(),
      headers: request.headers(),
      postData: request.postData() || null,
    });
  });

  // Intercept all responses
  page.on("response", async (response) => {
    const request = response.request();
    const contentType = response.headers()["content-type"] || "";
    const isJson = contentType.includes("json");
    const isText =
      contentType.includes("text") ||
      contentType.includes("xml") ||
      contentType.includes("html") ||
      contentType.includes("javascript");

    let body = null;
    try {
      if (isJson || isText) {
        body = await response.text();
      } else {
        const buf = await response.body();
        body = `base64:${buf.toString("base64")}`;
      }
    } catch {
      body = null;
    }

    captured.push({
      id: request._captureId,
      type: "response",
      timestamp: new Date().toISOString(),
      status: response.status(),
      url: response.url(),
      headers: response.headers(),
      contentType,
      body,
    });
  });

  await page.goto(startUrl, { waitUntil: "domcontentloaded" });

  // Wait for browser to close
  await new Promise((resolve) => {
    browser.on("disconnected", resolve);
    process.on("SIGINT", async () => {
      console.log("\nCtrl+C received, saving capture...");
      await browser.close().catch(() => {});
      resolve();
    });
  });

  // Save capture
  const timestamp = new Date().toISOString().replace(/[:.]/g, "-").slice(0, 19);
  const outDir = path.resolve(import.meta.dirname, "..", "captures");
  fs.mkdirSync(outDir, { recursive: true });
  const outFile = path.join(outDir, `${bankName}-${timestamp}.json`);

  const output = {
    bank: bankName,
    startUrl,
    capturedAt: new Date().toISOString(),
    entries: captured,
  };

  fs.writeFileSync(outFile, JSON.stringify(output, null, 2));
  console.log(`\nSaved ${captured.length} entries to ${outFile}`);
}

main().catch((err) => {
  console.error("Error:", err);
  process.exit(1);
});
