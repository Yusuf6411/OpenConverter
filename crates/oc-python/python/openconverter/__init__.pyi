from os import PathLike
from typing import Any, Dict, List, Optional, Union

__version__: str

class ConversionError(Exception):
    code: str
    detail: Optional[str]

class Result:
    input_format: str
    output_format: str
    route: str
    bytes_in: int
    bytes_out: int
    notes: List[Dict[str, Optional[str]]]

def convert(
    src: Union[str, "PathLike[str]"],
    dst: Union[str, "PathLike[str]"],
    *,
    overwrite: bool = False,
    **options: Any,
) -> Result: ...
def info(path: Union[str, "PathLike[str]"]) -> Dict[str, Any]: ...
def formats() -> List[Dict[str, Any]]: ...
