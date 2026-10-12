/** Admission closes synchronously; cleanup starts only after admitted work settles. */
export class DrainingLifecycle {
  private closing = false;
  private readonly active = new Set<Promise<unknown>>();
  private closed?: Promise<void>;

  run<T>(operation: () => Promise<T>): Promise<T> {
    if (this.closing) return Promise.reject(new Error("Plasm instance is closing or closed"));
    let resolve!: (value: T | PromiseLike<T>) => void;
    let reject!: (error: unknown) => void;
    const task = new Promise<T>((done, fail) => { resolve = done; reject = fail; });
    this.active.add(task);
    // Observe both outcomes without creating an unhandled rejected cleanup promise.
    void task.then(() => this.active.delete(task), () => this.active.delete(task));
    // Register before synchronous entry; input snapshots and reentrant close
    // retain the same ordering as the public operation's original async entry.
    try { resolve(operation()); } catch (error) { reject(error); }
    return task;
  }

  close(cleanup: () => Promise<void>): Promise<void> {
    this.closing = true;
    this.closed ??= Promise.allSettled([...this.active]).then(cleanup);
    return this.closed;
  }
}
