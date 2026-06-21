import { defineChannel } from "../../src/authoring/define-channel.js";

export default defineChannel({
  name: "health",
  routes: [
    {
      method: "GET",
      path: "/channel/health",
      handler: (_req, res) => {
        res.statusCode = 200;
        res.setHeader("content-type", "application/json; charset=utf-8");
        res.end(JSON.stringify({ status: "ok", channel: "health" }));
      },
    },
  ],
});
