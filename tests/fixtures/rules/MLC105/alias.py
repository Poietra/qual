import manim as mn


class Demo(mn.Scene):
    def construct(self):
        square = mn.Square()
        square.add_updater(lambda dt: dt)
