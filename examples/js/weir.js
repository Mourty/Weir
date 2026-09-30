// A small client for Weir's control socket, for Node.js 18 or later, with
// no dependencies. Copy this file next to your script or plugin.
//
//   const weir = require("./weir");
//
//   const mixer = await weir.connect();
//   await mixer.call("set_strip", { id: "Mic", mute: "toggle" });
//
//   mixer.on("state_changed", (state) => {
//     console.log("Mic muted:", weir.find(state.strips, "Mic").mute);
//   });
//   await mixer.subscribe(["state"]);
//
// The protocol is documented in docs/API.md.

"use strict";

const net = require("node:net");
const os = require("node:os");
const path = require("node:path");
const { EventEmitter } = require("node:events");

/** The daemon refused a request. `code` says what kind of problem it was
 * (see "Errors" in docs/API.md); the message says what exactly. */
class WeirError extends Error {
  constructor(code, message, data) {
    super(message);
    this.name = "WeirError";
    this.code = code;
    this.data = data;
  }
}

/** Where the daemon listens unless told otherwise. */
function defaultSocketPath() {
  const runtime = process.env.XDG_RUNTIME_DIR;
  if (runtime) return path.join(runtime, "weir", "control.sock");
  return `/tmp/weir-${os.userInfo().uid}/control.sock`;
}

/** The strip, bus or application in `items` with this id or name (names
 * ignoring case, as the daemon compares them), or undefined. */
function find(items, key) {
  return items.find(
    (item) =>
      item.id === key ||
      (typeof key === "string" && item.name.toLowerCase() === key.toLowerCase()),
  );
}

/** A bus's short label, as the mixer shows it: A1, A2... for buses that play
 * to a device and B1, B2... for virtual ones, counted in the order `buses`
 * lists them. Requests accept these in place of an id. */
function busLabel(buses, bus) {
  const sameKind = buses.filter((b) => b.kind === bus.kind).map((b) => b.id);
  const letter = bus.kind === "hardware" ? "A" : "B";
  return `${letter}${sameKind.indexOf(bus.id) + 1}`;
}

/**
 * One connection to the daemon.
 *
 * Every notification is emitted twice: under its own name, such as
 * `"state_changed"` with its params, and as `"notification"` with the name
 * and the params. `"close"` is emitted when the connection ends.
 */
class Weir extends EventEmitter {
  constructor(socket) {
    super();
    this.socket = socket;
    this.nextId = 1;
    this.pending = new Map();
    this.buffer = "";
    socket.setEncoding("utf8");
    socket.on("data", (chunk) => this.receive(chunk));
    socket.on("close", () => {
      const gone = new Error("the Weir daemon closed the connection");
      for (const { reject } of this.pending.values()) reject(gone);
      this.pending.clear();
      this.emit("close");
    });
  }

  /** Call `method` and resolve with its result, or reject with a WeirError. */
  call(method, params) {
    const id = this.nextId++;
    return new Promise((resolve, reject) => {
      this.pending.set(id, { resolve, reject });
      this.write(request(method, params, id));
    });
  }

  /** Send a request without waiting for, or getting, an answer. Good for a
   * stream of changes, such as a knob being turned. */
  send(method, params) {
    this.write(request(method, params));
  }

  /** Send several `[method, params]` requests in one message, run one after
   * the other. Resolves with one entry per request: its result, or the
   * WeirError it failed with. A failed request does not stop the rest. */
  batch(calls) {
    const ids = calls.map(() => this.nextId++);
    const answers = ids.map(
      (id) =>
        new Promise((resolve) => {
          this.pending.set(id, { resolve, reject: resolve });
        }),
    );
    this.write(calls.map(([method, params], i) => request(method, params, ids[i])));
    return Promise.all(answers);
  }

  /** Start getting notifications for these topics; all of them when the
   * list is empty. Resolves with the topics now active. */
  subscribe(topics = [], meterRateHz) {
    const params = { topics };
    if (meterRateHz !== undefined) params.meter_rate_hz = meterRateHz;
    return this.call("subscribe", params);
  }

  /** Close the connection. Subscriptions end with it. */
  close() {
    this.socket.end();
  }

  write(message) {
    this.socket.write(JSON.stringify(message) + "\n");
  }

  receive(chunk) {
    this.buffer += chunk;
    let newline;
    while ((newline = this.buffer.indexOf("\n")) >= 0) {
      const line = this.buffer.slice(0, newline);
      this.buffer = this.buffer.slice(newline + 1);
      if (line.trim()) this.dispatch(JSON.parse(line));
    }
  }

  dispatch(message) {
    if (Array.isArray(message)) {
      for (const answer of message) this.dispatch(answer);
      return;
    }
    if (message.method !== undefined) {
      this.emit(message.method, message.params);
      this.emit("notification", message.method, message.params);
      return;
    }
    const waiting = this.pending.get(message.id);
    if (!waiting) return;
    this.pending.delete(message.id);
    if (message.error) {
      const { code, message: text, data } = message.error;
      waiting.reject(new WeirError(code, text, data));
    } else {
      waiting.resolve(message.result);
    }
  }
}

function request(method, params, id) {
  const message = { jsonrpc: "2.0", method };
  if (id !== undefined) message.id = id;
  if (params !== undefined) message.params = params;
  return message;
}

/** Connect to the daemon, at `socketPath` or where it listens by default. */
function connect(socketPath = defaultSocketPath()) {
  return new Promise((resolve, reject) => {
    const socket = net.createConnection(socketPath);
    socket.once("connect", () => {
      socket.off("error", reject);
      resolve(new Weir(socket));
    });
    socket.once("error", reject);
  });
}

module.exports = { busLabel, connect, defaultSocketPath, find, Weir, WeirError };
