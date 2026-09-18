/** Preserve user-authored requests independently of the model's discovery focus. */
export class DiscoveryIntent {
  private requests: string[] = [];
  addRequest(request: string, reset: boolean): void {
    if (reset) this.requests = [];
    this.requests.push(request);
  }
  forCapabilityRequest(capabilityRequest: string): { intent: string; userRequests: string[] } {
    return { intent: capabilityRequest, userRequests: [...this.requests] };
  }
}
