import fs from "fs";

const args = process.argv.slice(2);
const flags = args.filter((a) => a.startsWith("--"));
const files = args.filter((a) => !a.startsWith("--"));

if (files.length === 0) {
  console.error("Usage: node analyze.mjs <capture.json> [--api-only] [--find-bankid] [--endpoints] [--cookies]");
  process.exit(1);
}

const STATIC_EXTENSIONS = /\.(js|css|png|jpg|jpeg|gif|svg|ico|woff|woff2|ttf|eot|map)(\?|$)/i;
const STATIC_TYPES = new Set(["stylesheet", "image", "font", "media", "manifest", "other"]);

// Load and merge all capture files
const allEntries = [];
for (const file of files) {
  const data = JSON.parse(fs.readFileSync(file, "utf-8"));
  allEntries.push(...data.entries);
}

// Build request/response pairs
const requests = allEntries.filter((e) => e.type === "request");
const responses = allEntries.filter((e) => e.type === "response");

const pairs = requests.map((req) => {
  const res = responses.find((r) => r.id === req.id);
  return { request: req, response: res || null };
});

function isApiRequest(pair) {
  const { request } = pair;
  if (STATIC_TYPES.has(request.resourceType)) return false;
  if (STATIC_EXTENSIONS.test(request.url)) return false;
  return true;
}

function urlPath(url) {
  try {
    return new URL(url).pathname;
  } catch {
    return url;
  }
}

// --api-only: Filter out static assets, show API calls
if (flags.includes("--api-only")) {
  console.log("=== API Requests ===\n");
  const apiPairs = pairs.filter(isApiRequest);

  for (const { request, response } of apiPairs) {
    const status = response ? response.status : "???";
    const contentType = response?.contentType || "";
    console.log(`${request.method} ${status} ${urlPath(request.url)}`);
    if (contentType) console.log(`  Content-Type: ${contentType}`);
    if (request.postData) {
      const preview = request.postData.slice(0, 200);
      console.log(`  Body: ${preview}${request.postData.length > 200 ? "..." : ""}`);
    }
    console.log();
  }

  console.log(`Total API requests: ${apiPairs.length}`);
}

// --find-bankid: Find BankID-related requests/responses
if (flags.includes("--find-bankid")) {
  console.log("=== BankID-related entries ===\n");
  const bankidPatterns = /bankid|autostart|qrstart|collect|authenticate|sign/i;

  for (const { request, response } of pairs) {
    const urlMatch = bankidPatterns.test(request.url);
    const bodyMatch = request.postData && bankidPatterns.test(request.postData);
    const resBodyMatch = response?.body && typeof response.body === "string" && bankidPatterns.test(response.body);

    if (urlMatch || bodyMatch || resBodyMatch) {
      const status = response ? response.status : "???";
      console.log(`${request.method} ${status} ${request.url}`);
      if (request.postData) {
        console.log(`  Request body: ${request.postData.slice(0, 500)}`);
      }
      if (response?.body && typeof response.body === "string" && !response.body.startsWith("base64:")) {
        console.log(`  Response body: ${response.body.slice(0, 500)}`);
      }
      console.log();
    }
  }
}

// --endpoints: Group by URL path
if (flags.includes("--endpoints")) {
  console.log("=== Endpoints ===\n");
  const apiPairs = pairs.filter(isApiRequest);
  const grouped = new Map();

  for (const pair of apiPairs) {
    const p = urlPath(pair.request.url);
    if (!grouped.has(p)) grouped.set(p, []);
    grouped.get(p).push(pair);
  }

  // Sort by first occurrence
  const sorted = [...grouped.entries()].sort((a, b) => {
    const aTime = a[1][0].request.timestamp;
    const bTime = b[1][0].request.timestamp;
    return aTime.localeCompare(bTime);
  });

  for (const [endpoint, endpointPairs] of sorted) {
    const methods = [...new Set(endpointPairs.map((p) => p.request.method))];
    const statuses = [...new Set(endpointPairs.map((p) => p.response?.status).filter(Boolean))];
    console.log(`${methods.join(",")} ${endpoint}  (${endpointPairs.length}x, status: ${statuses.join(",")})`);
  }
}

// --cookies: Show Set-Cookie headers over time
if (flags.includes("--cookies")) {
  console.log("=== Cookies (Set-Cookie headers) ===\n");

  for (const { request, response } of pairs) {
    if (!response) continue;
    const setCookie = response.headers["set-cookie"];
    if (!setCookie) continue;

    console.log(`${response.timestamp} ${urlPath(request.url)}`);
    const cookies = Array.isArray(setCookie) ? setCookie : [setCookie];
    for (const c of cookies) {
      // Show just cookie name=value (first part before ;)
      const nameValue = c.split(";")[0];
      console.log(`  ${nameValue}`);
    }
    console.log();
  }
}

// If no specific flag, show summary
if (flags.length === 0) {
  console.log("=== Capture Summary ===\n");
  console.log(`Total entries: ${allEntries.length}`);
  console.log(`Requests: ${requests.length}`);
  console.log(`Responses: ${responses.length}`);
  console.log(`API requests: ${pairs.filter(isApiRequest).length}`);
  console.log(`Static requests: ${pairs.filter((p) => !isApiRequest(p)).length}`);
  console.log("\nUse --api-only, --find-bankid, --endpoints, or --cookies for detailed views.");
}
