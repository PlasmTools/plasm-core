/** Serialize session discovery from snapshot acquisition through durable teaching
 * publication. Only identical, still-pending requests share a result; completed
 * requests are never cached because subsequent execution may change discovery needs.
 */
export class DiscoveryQueue {
  private readonly sessions = new Map<string, {
    tail: Promise<void>;
    pending: Map<string, Promise<string>>;
  }>();

  run(session: string, request: string, operation: () => Promise<string>): Promise<string> {
    let queue = this.sessions.get(session);
    if (!queue) {
      queue = { tail: Promise.resolve(), pending: new Map() };
      this.sessions.set(session, queue);
    }
    const existing = queue.pending.get(request);
    if (existing) return existing;

    const result = queue.tail.then(operation);
    queue.pending.set(request, result);
    const settle = () => {
      queue.pending.delete(request);
      if (queue.pending.size === 0) this.sessions.delete(session);
    };
    // Both outcomes release admission. The caller still receives the original
    // rejection, while later requests start from the last persisted state.
    queue.tail = result.then(settle, settle);
    return result;
  }
}
