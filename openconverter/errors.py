"""Typed exceptions and the error-code table shared by every layer."""


class OpenConverterError(Exception):
    """Base class. ``code`` is the stable machine-readable error code."""

    code = "internal_error"
    http_status = 500

    def __init__(self, message: str = "") -> None:
        super().__init__(message or self.default_message)

    default_message = "An unexpected error occurred."

    @property
    def message(self) -> str:
        return str(self)


class BadRequest(OpenConverterError):
    code = "bad_request"
    http_status = 400
    default_message = "The request was invalid."


class Forbidden(OpenConverterError):
    code = "forbidden"
    http_status = 403
    default_message = "The request was rejected by a security check."


class NotFound(OpenConverterError):
    code = "not_found"
    http_status = 404
    default_message = "Not found."


class MethodNotAllowed(OpenConverterError):
    code = "method_not_allowed"
    http_status = 405
    default_message = "Method not allowed."


class LimitExceeded(OpenConverterError):
    code = "limit_exceeded"
    http_status = 413
    default_message = "A size limit was exceeded."


class UnsupportedFormat(OpenConverterError):
    code = "unsupported_format"
    http_status = 415
    default_message = "This file format is not supported."


class UnsupportedVariant(OpenConverterError):
    code = "unsupported_variant"
    http_status = 422
    default_message = "This variant of the format is not supported."


class CorruptFile(OpenConverterError):
    code = "corrupt_file"
    http_status = 422
    default_message = "The file is damaged or not a valid file of its format."


class ConversionCancelled(OpenConverterError):
    code = "cancelled"
    http_status = 409
    default_message = "The conversion was cancelled."


class ServiceUnavailable(OpenConverterError):
    code = "service_unavailable"
    http_status = 503
    default_message = "The server is busy; try again shortly."


ERROR_TABLE = {
    cls.code: cls
    for cls in (
        OpenConverterError,
        BadRequest,
        Forbidden,
        NotFound,
        MethodNotAllowed,
        LimitExceeded,
        UnsupportedFormat,
        UnsupportedVariant,
        CorruptFile,
        ConversionCancelled,
        ServiceUnavailable,
    )
}


def error_body(exc: BaseException) -> dict:
    """JSON body for an error response. Never includes a traceback."""
    if isinstance(exc, OpenConverterError):
        return {"error": {"code": exc.code, "message": exc.message}}
    return {"error": {"code": "internal_error", "message": OpenConverterError.default_message}}
