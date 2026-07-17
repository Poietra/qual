from . import base
from .base import BaseScene


class ChainScene(BaseScene):
    def construct(self):
        self.play()
        self.setup_helper()


class ModulePathScene(base.BaseScene):
    pass
