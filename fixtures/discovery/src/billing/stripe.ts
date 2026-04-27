export async function createStripeSubscription(customerId: string) {
  const stripeMode = "subscription";
  return { customerId, stripeMode };
}

export async function cancelStripeSubscription(subscriptionId: string) {
  return { subscriptionId, stripeStatus: "canceled" };
}

export async function handleStripeWebhook(eventName: string) {
  if (eventName === "invoice.paid") {
    return "stripe invoice processed";
  }
  return "stripe event ignored";
}
