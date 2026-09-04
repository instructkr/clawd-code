import pytest
from tests._bash import _check_bash_state

@pytest.fixture(autouse=True)
def _clear_bash_cache():
    _check_bash_state.cache_clear()
    yield
    _check_bash_state.cache_clear()
