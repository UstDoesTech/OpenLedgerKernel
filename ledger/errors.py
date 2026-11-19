class LedgerError(Exception):
    """Base class for ledger errors."""


class Unreachable(LedgerError):
    """A node (or the control plane) cannot be reached from here."""


class NotHome(LedgerError):
    """This node is not the home for the account, so it may not write to it."""


class LeaseExpired(LedgerError):
    """The home's lease is not valid (expired, or the control plane is unreachable)."""


class Fenced(LedgerError):
    """The writer's epoch is older than the epoch the database knows about."""


class InsufficientFunds(LedgerError):
    pass


class IdempotencyConflict(LedgerError):
    """Same transfer id reused with different details."""


class DurabilityNotMet(LedgerError):
    """Committed locally, but the strong tier could not get a remote apply ack."""
