export default {
  id: "tinyfish",
  alias: "tinyfish",
  display: {
    name: "TinyFish",
    icon: "travel_explore",
    color: "#0EA5A4",
    textIcon: "TF",
    website: "https://tinyfish.ai",
    notice: {
      apiKeyUrl: "https://agent.tinyfish.ai/api-keys"
    }
  },
  category: "apikey",
  authType: "apikey",
  // Both Search and Fetch are free on every TinyFish plan — they never draw
  // from the wallet — so costPerQuery is 0 rather than absent. That matters:
  // it shows up in the dashboard's usage breakdown as a real number, and
  // `freeMonthlyQuota` is what the provider card renders.
  serviceKinds: [
    "webSearch",
    "webFetch"
  ],
  searchConfig: {
    // Search is a GET with query parameters, unlike most search providers here
    // which are POST+JSON. fetchSearchRequest honours that.
    baseUrl: "https://api.search.tinyfish.ai",
    method: "GET",
    authType: "apikey",
    // TinyFish authenticates with X-API-Key, not Authorization: Bearer. The
    // request builder sends the header the config declares, so the key-validation
    // probe and the real call agree.
    authHeader: "x-api-key",
    costPerQuery: 0,
    freeMonthlyQuota: 0,
    // TinyFish calls these domain_type, and supports research_paper as a third
    // kind alongside our web/news.
    searchTypes: [
      "web",
      "news",
      "research_paper"
    ],
    defaultMaxResults: 5,
    maxMaxResults: 20,
    timeoutMs: 10000,
  },
  fetchConfig: {
    baseUrl: "https://api.fetch.tinyfish.ai",
    method: "POST",
    authType: "apikey",
    authHeader: "x-api-key",
    costPerQuery: 0,
    freeMonthlyQuota: 0,
    formats: [
      "markdown",
      "html",
      "json"
    ],
    maxCharacters: 100000,
    timeoutMs: 30000,
  }
};
