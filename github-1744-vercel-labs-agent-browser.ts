// File: src/tools/paid-x402-endpoints.ts
import { Tool } from '../types/tool';

/**
 * Implements the 11 paid x402 endpoints as specified in the bounty issue
 * These endpoints allow the agent-browser to interact with paid services via x402 protocol
 */
export const paidX402Endpoints: Tool[] = [
  {
    name: 'x402_pay_for_service',
    description: 'Pay for a service using x402 protocol with specified amount and currency',
    parameters: {
      type: 'object' as const,
      properties: {
        serviceUrl: { type: 'string', description: 'URL of the service to pay for' },
        amount: { type: 'number', description: 'Amount to pay' },
        currency: { type: 'string', description: 'Currency code (e.g., USD, EUR)' },
        metadata: { type: 'object', description: 'Additional payment metadata' }
      },
      required: ['serviceUrl', 'amount', 'currency']
    },
    handler: async ({ serviceUrl, amount, currency, metadata }) => {
      try {
        const response = await fetch(serviceUrl, {
          method: 'POST',
          headers: {
            'Content-Type': 'application/json',
            'X-Payment-Amount': amount.toString(),
            'X-Payment-Currency': currency
          },
          body: JSON.stringify(metadata || {})
        });
        
        if (!response.ok) {
          throw new Error(`Payment failed: ${response.status} ${response.statusText}`);
        }
        
        return await response.json();
      } catch (error) {
        return { error: 'Payment failed', details: error instanceof Error ? error.message : String(error) };
      }
    }
  },
  {
    name: 'x402_check_balance',
    description: 'Check the current balance for a service endpoint',
    parameters: {
      type: 'object' as const,
      properties: {
        serviceUrl: { type: 'string', description: 'URL of the service to check balance for' }
      },
      required: ['serviceUrl']
    },
    handler: async ({ serviceUrl }) => {
      try {
        const response = await fetch(serviceUrl, {
          method: 'GET',
          headers: { 'X-Balance-Check': 'true' }
        });
        
        if (!response.ok) {
          throw new Error(`Balance check failed: ${response.status} ${response.statusText}`);
        }
        
        const balance = response.headers.get('X-Available-Balance');
        return { 
          serviceUrl, 
          balance: balance ? parseFloat(balance) : null,
          currency: response.headers.get('X-Currency') || 'USD'
        };
      } catch (error) {
        return { error: 'Balance check failed', details: error instanceof Error ? error.message : String(error) };
      }
    }
  },
  {
    name: 'x402_authorize_payment',
    description: 'Pre-authorize a payment for later execution',
    parameters: {
      type: 'object' as const,
      properties: {
        serviceUrl: { type: 'string', description: 'URL of the service' },
        amount: { type: 'number', description: 'Amount to authorize' },
        currency: { type: 'string', description: 'Currency code' },
        authorizationId: { type: 'string', description: 'Unique ID for this authorization' }
      },
      required: ['serviceUrl', 'amount', 'currency', 'authorizationId']
    },
    handler: async ({ serviceUrl, amount, currency, authorizationId }) => {
      try {
        const response = await fetch(serviceUrl, {
          method: 'POST',
          headers: {
            'Content-Type': 'application/json',
            'X-Authorization-ID': authorizationId,
            'X-Payment-Amount': amount.toString(),
            'X-Payment-Currency': currency
          },
          body: JSON.stringify({ action: 'authorize' })
        });
        
        if (!response.ok) {
          throw new Error(`Authorization failed: ${response.status} ${response.statusText}`);
        }
        
        return await response.json();
      } catch (error) {
        return { error: 'Authorization failed', details: error instanceof Error ? error.message : String(error) };
      }
    }
  },
  {
    name: 'x402_cancel_authorization',
    description: 'Cancel a previously authorized payment',
    parameters: {
      type: 'object' as const,
      properties: {
        serviceUrl: { type: 'string', description: 'URL of the service' },
        authorizationId: { type: 'string', description: 'ID of the authorization to cancel' }
      },
      required: ['serviceUrl', 'authorizationId']
    },
    handler: async ({ serviceUrl, authorizationId }) => {
      try {
        const response = await fetch(serviceUrl, {
          method: 'DELETE',
          headers: {
            'X-Authorization-ID': authorizationId
          }
        });
        
        if (!response.ok) {
          throw new Error(`Authorization cancellation failed: ${response.status} ${response.statusText}`);
        }
        
        return { success: true, message: 'Authorization cancelled' };
      } catch (error) {
        return { error: 'Authorization cancellation failed', details: error instanceof Error ? error.message : String(error) };
      }
    }
  },
  {
    name: 'x402_refund_payment',
    description: 'Request a refund for a previous payment',
    parameters: {
      type: 'object' as const,
      properties: {
        serviceUrl: { type: 'string', description: 'URL of the service' },
        originalTransactionId: { type: 'string', description: 'ID of the original transaction' },
        amount: { type: 'number', description: 'Amount to refund' }
      },
      required: ['serviceUrl', 'originalTransactionId']
    },
    handler: async ({ serviceUrl, originalTransactionId, amount }) => {
      try {
        const response = await fetch(serviceUrl, {
          method: 'POST',
          headers: {
            'Content-Type': 'application/json',
            'X-Original-Transaction-ID': originalTransactionId,
            'X-Refund-Amount': amount ? amount.toString() : 'full'
          }
        });
        
        if (!response.ok) {
          throw new Error(`Refund failed: ${response.status} ${response.statusText}`);
        }
        
        return await response.json();
      } catch (error) {
        return { error: 'Refund failed', details: error instanceof Error ? error.message : String(error) };
      }
    }
  },
  {
    name: 'x402_get_transaction_history',
    description: 'Retrieve transaction history for a service',
    parameters: {
      type: 'object' as const,
      properties: {
        serviceUrl: { type: 'string', description: 'URL of the service' },
        limit: { type: 'number', description: 'Maximum number of transactions to return' },
        offset: { type: 'number', description: 'Offset for pagination' }
      },
      required: ['serviceUrl']
    },
    handler: async ({ serviceUrl, limit = 10, offset = 0 }) => {
      try {
        const response = await fetch(`${serviceUrl}?limit=${limit}&offset=${offset}`, {
          method: 'GET',
          headers: { 'X-Transaction-History': 'true' }
        });
        
        if (!response.ok) {
          throw new Error(`Transaction history fetch failed: ${response.status} ${response.statusText}`);
        }
        
        return await response.json();
      } catch (error) {
        return { error: 'Transaction history fetch failed', details: error instanceof Error ? error.message : String(error) };
      }
    }
  },
  {
    name: 'x402_validate_payment',
    description: 'Validate that a payment was processed correctly',
    parameters: {
      type: 'object' as const,
      properties: {
        serviceUrl: { type: 'string', description: 'URL of the service' },
        transactionId: { type: 'string', description: 'ID of the transaction to validate' }
      },
      required: ['serviceUrl', 'transactionId']
    },
    handler: async ({ serviceUrl, transactionId }) => {
      try {
        const response = await fetch(`${serviceUrl}/validate/${transactionId}`, {
          method: 'GET',
          headers: { 'X-Validation-Request': 'true' }
        });
        
        if (!response.ok) {
          throw new Error(`Validation failed: ${response.status} ${response.statusText}`);
        }
        
        return await response.json();
      } catch (error) {
        return { error: 'Validation failed', details: error instanceof Error ? error.message : String(error) };
      }
    }
  },
  {
    name: 'x402_setup_recurring_payment',
    description: 'Set up a recurring payment schedule',
    parameters: {
      type: 'object' as const,
      properties: {
        serviceUrl: { type: 'string', description: 'URL of the service' },
        amount: { type: 'number', description: 'Payment amount' },
        currency: { type: 'string', description: 'Currency code' },
        frequency: { type: 'string', description: 'Payment frequency (daily, weekly, monthly, yearly)' },
        startDate: { type: 'string', description: 'Start date in ISO format' },
        endDate: { type: 'string', description: 'End date in ISO format (optional)' }
      },
      required: ['serviceUrl', 'amount', 'currency', 'frequency', 'startDate']
    },
    handler: async ({ serviceUrl, amount, currency, frequency, startDate, endDate }) => {
      try {
        const response = await fetch(serviceUrl, {
          method: 'POST',
          headers: {
            'Content-Type': 'application/json',
            'X-Recurring-Payment': 'true',
            'X-Payment-Amount': amount.toString(),
            'X-Payment-Currency': currency,
            'X-Frequency': frequency,
            'X-Start-Date': startDate
          },
          body: JSON.stringify({ endDate })
        });
        
        if (!response.ok) {
          throw new Error(`Recurring payment setup failed: ${response.status} ${response.statusText}`);
        }
        
        return await response.json();
      } catch (error) {
        return { error: 'Recurring payment setup failed', details: error instanceof Error ? error.message : String(error) };
      }
    }
  },
  {
    name: 'x402_cancel_recurring_payment',
    description: 'Cancel an existing recurring payment',
    parameters: {
      type: 'object' as const,
      properties: {
        serviceUrl: { type: 'string', description: 'URL of the service' },
        recurringPaymentId: { type: 'string', description: 'ID of the recurring payment' }
      },
      required: ['serviceUrl', 'recurringPaymentId']
    },
    handler: async ({ serviceUrl, recurringPaymentId }) => {
      try {
        const response = await fetch(serviceUrl, {
          method: 'DELETE',
          headers: {
            'X-Recurring-Payment-ID': recurringPaymentId
          }
        });
        
        if (!response.ok) {
          throw new Error(`Recurring payment cancellation failed: ${response.status} ${response.statusText}`);
        }
        
        return { success: true, message: 'Recurring payment cancelled' };
      } catch (error) {
        return { error: 'Recurring payment cancellation failed', details: error instanceof Error ? error.message : String(error) };
      }
    }
  },
  {
    name: 'x402_get_payment_methods',
    description: 'Retrieve available payment methods for a service',
    parameters: {
      type: 'object' as const,
      properties: {
        serviceUrl: { type: 'string', description: 'URL of the service' }
      },
      required: ['serviceUrl']
    },
    handler: async ({ serviceUrl }) => {
      try {
        const response = await fetch(serviceUrl, {
          method: 'GET',
          headers: { 'X-Payment-Methods': 'true' }
        });
        
        if (!response.ok) {
          throw new Error(`Payment methods fetch failed: ${response.status} ${response.statusText}`);
        }
        
        return await response.json();
      } catch (error) {
        return { error: 'Payment methods fetch failed', details: error instanceof Error ? error.message : String(error) };
      }
    }
  },
  {
    name: 'x402_update_payment_method',
    description: 'Update the payment method for a service',
    parameters: {
      type: 'object' as const,
      properties: {
        serviceUrl: { type: 'string', description: 'URL of the service' },
        paymentMethodId: { type: 'string', description: 'ID of the new payment method' }
      },
      required: ['serviceUrl', 'paymentMethodId']
    },
    handler: async ({ serviceUrl, paymentMethodId }) => {
      try {
        const response = await fetch(serviceUrl, {
          method: 'PATCH',
          headers: {
            'Content-Type': 'application/json',
            'X-Payment-Method-ID': paymentMethodId
          }
        });
        
        if (!response.ok) {
          throw new Error(`Payment method update failed: ${response.status} ${response.statusText}`);
        }
        
        return await response.json();
      } catch (error) {
        return { error: 'Payment method update failed', details: error instanceof Error ? error.message : String(error) };
      }
    }
  }
];