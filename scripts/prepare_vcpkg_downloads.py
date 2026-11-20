"""Normalize pinned vcpkg GitLab base URLs without changing source/hash pins."""
from pathlib import Path
import argparse


def prepare(root: Path):
    helper = root.resolve(strict=True) / 'scripts/cmake/vcpkg_from_gitlab.cmake'
    text = helper.read_text(encoding='utf-8')
    normalization = '    string(REGEX REPLACE "/+$" "" arg_GITLAB_URL "${arg_GITLAB_URL}")\n'
    anchor = '    if(NOT DEFINED arg_GITLAB_URL)\n        message(FATAL_ERROR "GITLAB_URL must be specified.")\n    endif()\n'
    if normalization not in text:
        if text.count(anchor) != 1:
            raise RuntimeError('Pinned vcpkg GitLab helper changed; review URL normalization')
        helper.write_text(text.replace(anchor, anchor+'\n'+normalization), encoding='utf-8', newline='\n')
    print('Normalized vcpkg GitLab base URLs; source revisions and SHA512 checks remain unchanged')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('root', type=Path)
    prepare(parser.parse_args().root)
