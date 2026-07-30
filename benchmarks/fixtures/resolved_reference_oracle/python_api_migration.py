from typing import Protocol


class CloseProtocol(Protocol):
    def close(self) -> None:
        ...


class Runtime(CloseProtocol):
    def close(self) -> None:
        pass


class RuntimeChild(Runtime):
    pass


class OtherRuntime:
    def close(self) -> None:
        pass


class RuntimeMock(Runtime):
    def close(self) -> None:
        pass


class RuntimeWrapper:
    def __init__(self, runtime: Runtime) -> None:
        self.runtime = runtime

    def close(self) -> None:
        self.runtime.close()


close = Runtime.close
compat_close = close


def migrate(
    runtime: Runtime,
    child: RuntimeChild,
    other: OtherRuntime,
    unknown,
) -> None:
    runtime.close()
    child.close()
    other.close()
    unknown.close()
    Runtime().close()
    OtherRuntime().close()
    marker = "runtime.close() is documentation, not a call"
    # runtime.close() in a comment is not executable
