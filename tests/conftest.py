from pyln.client import NodeVersion
import pytest
import subprocess


def get_cln_version():
    cln_version_proc = subprocess.check_output(["lightningd", "--version"])
    cln_version = NodeVersion(cln_version_proc.decode("ascii").strip())

    return cln_version


def pytest_configure(config):
    if not hasattr(config, "workerinput"):
        cln_version = get_cln_version()
        config.v2606 = cln_version >= NodeVersion("v26.06")
        config.experimental_splicing_required = cln_version < NodeVersion("v26.04")


def pytest_configure_node(node):
    node.workerinput["v2606"] = node.config.v2606
    node.workerinput["experimental_splicing_required"] = (
        node.config.experimental_splicing_required
    )


@pytest.fixture(scope="session")
def v2606(request):
    if hasattr(request.config, "workerinput"):
        return request.config.workerinput["v2606"]

    return request.config.v2606


@pytest.fixture(scope="session")
def experimental_splicing_required(request):
    if hasattr(request.config, "workerinput"):
        return request.config.workerinput["experimental_splicing_required"]

    return request.config.experimental_splicing_required
