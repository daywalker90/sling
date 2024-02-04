import string
import random
import logging
import os
import pytest
from pathlib import Path
import requests
import tarfile
import platform

VERSION = "v1.4.1"
RUST_PROFILE = os.environ.get("RUST_PROFILE", "debug")
COMPILED_PATH = Path.cwd() / "target" / RUST_PROFILE / \
    "sling"


@pytest.fixture
def get_plugin(directory):
    downloaded_plugin_path = Path(
        os.path.join(directory, "sling.tar.gz"))
    extracted_plugin_path = Path(os.path.join(directory, "sling"))
    if COMPILED_PATH.is_file():
        return COMPILED_PATH
    elif extracted_plugin_path.is_file():
        return extracted_plugin_path
    else:
        architecture = get_architecture()

        url = (f"https://github.com/daywalker90/sling/releases/download/"
               f"{VERSION}/sling-{VERSION}-{architecture}.tar.gz")
        response = requests.get(url)
        with open(downloaded_plugin_path, "wb") as file:
            file.write(response.content)

        with tarfile.open(downloaded_plugin_path, "r:gz") as tar:
            tar.extractall(directory)

        return extracted_plugin_path


def get_architecture():
    machine = platform.machine()

    if machine == 'x86_64':
        return 'x86_64-linux-gnu'
    elif machine == 'armv7l':
        return 'armv7-linux-gnueabihf'
    elif machine == 'aarch64':
        return 'aarch64-linux-gnu'
    else:
        raise RuntimeError(
            f"No self-compiled binary found and "
            f"unsupported release-architecture: {machine}")


def generate_random_label():
    label_length = 8
    random_label = ''.join(random.choice(string.ascii_letters)
                           for _ in range(label_length))
    return random_label


def generate_random_number():
    return random.randint(1, 20_000_000_000_000_00_000)


def pay_with_thread(rpc, bolt11):
    LOGGER = logging.getLogger(__name__)
    try:
        rpc.dev_pay(bolt11, dev_use_shadow=False)
    except Exception as e:
        LOGGER.debug(f"holdinvoice: Error paying payment hash:{e}")
        pass
