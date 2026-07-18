from manim import *


def get_flag():
    return unknown_source()


class AlwaysOn(Scene):
    def construct(self):
        self.always_update_mobjects = True
        self.wait()


class AlwaysOff(Scene):
    def construct(self):
        self.always_update_mobjects = False
        self.wait()


class AlwaysMaybe(Scene):
    def construct(self):
        self.always_update_mobjects = get_flag()
        self.wait()


class AlwaysInit(Scene):
    def __init__(self):
        super().__init__(always_update_mobjects=True)

    def construct(self):
        self.wait()
