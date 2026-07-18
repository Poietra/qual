from manim import RIGHT, ImageMobject, Scene, Square


class Demo(Scene):
    def construct(self):
        mover = Square()
        photo = ImageMobject("photo.png")
        self.add(mover, photo)
        depth = len(photo.submobjects)
        photo.set_z_index(depth)
        self.play(mover.animate.shift(RIGHT))
