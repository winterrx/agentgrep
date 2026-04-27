import { createStripeSubscription } from "../src/billing/stripe";

test("creates stripe subscription", async () => {
  const result = await createStripeSubscription("cus_123");
  expect(result.stripeMode).toBe("subscription");
});
